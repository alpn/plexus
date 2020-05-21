use itertools::Itertools;
use smallvec::SmallVec;
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};

use crate::graph::core::{Core, OwnedCore, RefCore};
use crate::graph::edge::{Arc, ArcKey, ArcView};
use crate::graph::face::{Face, FaceKey, FaceView};
use crate::graph::geometry::{Geometric, Geometry, GraphGeometry};
use crate::graph::mutation::edge::{self, ArcBridgeCache, EdgeMutation};
use crate::graph::mutation::vertex;
use crate::graph::mutation::{Consistent, Mutable, Mutation};
use crate::graph::vertex::{Vertex, VertexKey, VertexView};
use crate::graph::{GraphError, Ringoid};
use crate::network::borrow::Reborrow;
use crate::network::storage::{AsStorage, Fuse, Storage};
use crate::network::view::{Bind, ClosedView};
use crate::transact::Transact;
use crate::{DynamicArity, IteratorExt as _};

pub struct FaceMutation<M>
where
    M: Geometric,
{
    inner: EdgeMutation<M>,
    storage: Storage<Face<Geometry<M>>>,
}

impl<M, G> FaceMutation<M>
where
    M: Geometric<Geometry = G>,
    G: GraphGeometry,
{
    pub fn commit_unchecked(self) -> OwnedCore<G> {
        let FaceMutation {
            inner,
            storage: faces,
        } = self;
        inner.commit_unchecked().fuse(faces)
    }

    pub fn to_ref_core(&self) -> RefCore<G> {
        self.inner.to_ref_core().fuse(&self.storage)
    }

    // TODO: Should there be a distinction between `connect_face_to_arc` and
    //       `connect_arc_to_face`?
    pub fn connect_face_to_arc(&mut self, ab: ArcKey, abc: FaceKey) -> Result<(), GraphError> {
        self.with_face_mut(abc, |face| face.arc = ab)
    }

    fn connect_face_interior(&mut self, arcs: &[ArcKey], face: FaceKey) -> Result<(), GraphError> {
        for (ab, bc) in arcs.iter().cloned().perimeter() {
            self.connect_neighboring_arcs(ab, bc)?;
            self.connect_arc_to_face(ab, face)?;
        }
        Ok(())
    }

    fn disconnect_face_interior(&mut self, arcs: &[ArcKey]) -> Result<(), GraphError> {
        for ab in arcs {
            self.disconnect_arc_from_face(*ab)?;
        }
        Ok(())
    }

    fn connect_face_exterior(
        &mut self,
        arcs: &[ArcKey],
        connectivity: (
            HashMap<VertexKey, Vec<ArcKey>>,
            HashMap<VertexKey, Vec<ArcKey>>,
        ),
    ) -> Result<(), GraphError> {
        let (incoming, outgoing) = connectivity;
        for ab in arcs.iter().cloned() {
            let (a, b) = ab.into();
            let ba = ab.into_opposite();
            let neighbors = {
                let core = &self.to_ref_core();
                if ArcView::bind(core, ba)
                    .ok_or_else(|| GraphError::TopologyMalformed)?
                    .is_boundary_arc()
                {
                    // The next arc of BA is the outgoing arc of the destination
                    // vertex A that is also a boundary arc or, if there is no
                    // such outgoing arc, the next exterior arc of the face. The
                    // previous arc is similar.
                    let ax = outgoing[&a]
                        .iter()
                        .cloned()
                        .flat_map(|ax| ArcView::bind(core, ax))
                        .find(|next| next.is_boundary_arc())
                        .or_else(|| {
                            ArcView::bind(core, ab)
                                .and_then(|arc| arc.into_reachable_previous_arc())
                                .and_then(|previous| previous.into_reachable_opposite_arc())
                        })
                        .map(|next| next.key());
                    let xb = incoming[&b]
                        .iter()
                        .cloned()
                        .flat_map(|xb| ArcView::bind(core, xb))
                        .find(|previous| previous.is_boundary_arc())
                        .or_else(|| {
                            ArcView::bind(core, ab)
                                .and_then(|arc| arc.into_reachable_next_arc())
                                .and_then(|next| next.into_reachable_opposite_arc())
                        })
                        .map(|previous| previous.key());
                    ax.into_iter().zip(xb.into_iter()).next()
                }
                else {
                    None
                }
            };
            if let Some((ax, xb)) = neighbors {
                self.connect_neighboring_arcs(ba, ax)?;
                self.connect_neighboring_arcs(xb, ba)?;
            }
        }
        Ok(())
    }

    fn with_face_mut<T, F>(&mut self, abc: FaceKey, mut f: F) -> Result<T, GraphError>
    where
        F: FnMut(&mut Face<G>) -> T,
    {
        let face = self
            .storage
            .get_mut(&abc)
            .ok_or_else(|| GraphError::TopologyNotFound)?;
        Ok(f(face))
    }
}

impl<M, G> AsStorage<Face<G>> for FaceMutation<M>
where
    M: Geometric<Geometry = G>,
    G: GraphGeometry,
{
    fn as_storage(&self) -> &Storage<Face<G>> {
        &self.storage
    }
}

// TODO: This is a hack. Replace this with delegation.
impl<M> Deref for FaceMutation<M>
where
    M: Geometric,
{
    type Target = EdgeMutation<M>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<M> DerefMut for FaceMutation<M>
where
    M: Geometric,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<M, G> From<OwnedCore<G>> for FaceMutation<M>
where
    M: Geometric<Geometry = G>,
    G: GraphGeometry,
{
    fn from(core: OwnedCore<G>) -> Self {
        let (vertices, arcs, edges, faces) = core.unfuse();
        FaceMutation {
            storage: faces,
            inner: Core::empty().fuse(vertices).fuse(arcs).fuse(edges).into(),
        }
    }
}

impl<M, G> Transact<OwnedCore<G>> for FaceMutation<M>
where
    M: Geometric<Geometry = G>,
    G: GraphGeometry,
{
    type Output = OwnedCore<G>;
    type Error = GraphError;

    fn commit(self) -> Result<Self::Output, Self::Error> {
        let FaceMutation {
            inner,
            storage: faces,
        } = self;
        inner.commit().map(move |core| core.fuse(faces))
    }
}

pub struct FaceInsertCache {
    perimeter: SmallVec<[VertexKey; 4]>,
    connectivity: (
        HashMap<VertexKey, Vec<ArcKey>>,
        HashMap<VertexKey, Vec<ArcKey>>,
    ),
}

impl FaceInsertCache {
    pub fn snapshot<B, K>(storage: B, perimeter: K) -> Result<Self, GraphError>
    where
        B: Reborrow,
        B::Target: AsStorage<Arc<Geometry<B>>>
            + AsStorage<Face<Geometry<B>>>
            + AsStorage<Vertex<Geometry<B>>>
            + Geometric,
        K: IntoIterator,
        K::Item: Borrow<VertexKey>,
    {
        let perimeter = perimeter
            .into_iter()
            .map(|key| *key.borrow())
            .collect::<SmallVec<_>>();
        let arity = perimeter.len();
        let set = perimeter.iter().cloned().collect::<HashSet<_>>();
        if set.len() != arity {
            // Vertex keys are not unique.
            return Err(GraphError::TopologyMalformed);
        }

        let storage = storage.reborrow();
        let vertices = perimeter
            .iter()
            .cloned()
            .flat_map(|key| VertexView::bind(storage, key))
            .collect::<SmallVec<[_; 4]>>();
        if vertices.len() != arity {
            // Vertex keys refer to nonexistent vertices.
            return Err(GraphError::TopologyNotFound);
        }
        for (previous, next) in perimeter
            .iter()
            .cloned()
            .perimeter()
            .map(|keys| ArcView::bind(storage, keys.into()))
            .perimeter()
        {
            if let Some(previous) = previous {
                if previous.face.is_some() {
                    // A face already occupies an interior arc.
                    return Err(GraphError::TopologyConflict);
                }
                // Let the previous arc be AB and the next arc be BC. The
                // vertices A, B, and C lie within the implied ring in order.
                //
                // If BC does not exist and AB is neighbors with some arc BX,
                // then X must not lie within the implied ring (the ordered set
                // of vertices given to this function). If X is within the path,
                // then BX must bisect the implied ring (because X cannot be C).
                if next.is_none() {
                    if let Some(next) = previous.reachable_next_arc() {
                        let (_, destination) = next.key().into();
                        if set.contains(&destination) {
                            return Err(GraphError::TopologyConflict);
                        }
                    }
                }
            }
        }

        let mut incoming = HashMap::with_capacity(arity);
        let mut outgoing = HashMap::with_capacity(arity);
        for vertex in vertices {
            let key = vertex.key();
            incoming.insert(key, vertex.reachable_incoming_arcs().keys().collect());
            outgoing.insert(key, vertex.reachable_outgoing_arcs().keys().collect());
        }
        Ok(FaceInsertCache {
            perimeter,
            connectivity: (incoming, outgoing),
        })
    }
}

pub struct FaceRemoveCache {
    abc: FaceKey,
    arcs: Vec<ArcKey>,
}

impl FaceRemoveCache {
    // TODO: Should this require consistency?
    pub fn snapshot<B>(storage: B, abc: FaceKey) -> Result<Self, GraphError>
    where
        B: Reborrow,
        B::Target: AsStorage<Arc<Geometry<B>>>
            + AsStorage<Face<Geometry<B>>>
            + AsStorage<Vertex<Geometry<B>>>
            + Consistent
            + Geometric,
    {
        let face = FaceView::bind(storage, abc).ok_or_else(|| GraphError::TopologyNotFound)?;
        let arcs = face.interior_arcs().map(|arc| arc.key()).collect();
        Ok(FaceRemoveCache { abc, arcs })
    }
}

pub struct FaceSplitCache {
    cache: FaceRemoveCache,
    left: Vec<VertexKey>,
    right: Vec<VertexKey>,
}

impl FaceSplitCache {
    pub fn snapshot<B>(
        storage: B,
        abc: FaceKey,
        source: VertexKey,
        destination: VertexKey,
    ) -> Result<Self, GraphError>
    where
        B: Reborrow,
        B::Target: AsStorage<Arc<Geometry<B>>>
            + AsStorage<Face<Geometry<B>>>
            + AsStorage<Vertex<Geometry<B>>>
            + Consistent
            + Geometric,
    {
        let storage = storage.reborrow();
        let face = FaceView::bind(storage, abc).ok_or_else(|| GraphError::TopologyNotFound)?;
        face.distance(source.into(), destination.into())
            .and_then(|distance| {
                if distance <= 1 {
                    Err(GraphError::TopologyMalformed)
                }
                else {
                    Ok(())
                }
            })?;
        let perimeter = face
            .vertices()
            .map(|vertex| vertex.key())
            .collect::<Vec<_>>()
            .into_iter()
            .cycle();
        let left = perimeter
            .clone()
            .tuple_windows()
            .skip_while(|(_, b)| *b != source)
            .take_while(|(a, _)| *a != destination)
            .map(|(_, b)| b)
            .collect::<Vec<_>>();
        let right = perimeter
            .tuple_windows()
            .skip_while(|(_, b)| *b != destination)
            .take_while(|(a, _)| *a != source)
            .map(|(_, b)| b)
            .collect::<Vec<_>>();
        Ok(FaceSplitCache {
            cache: FaceRemoveCache::snapshot(storage, abc)?,
            left,
            right,
        })
    }
}

pub struct FacePokeCache {
    vertices: Vec<VertexKey>,
    cache: FaceRemoveCache,
}

impl FacePokeCache {
    pub fn snapshot<B>(storage: B, abc: FaceKey) -> Result<Self, GraphError>
    where
        B: Reborrow,
        B::Target: AsStorage<Arc<Geometry<B>>>
            + AsStorage<Face<Geometry<B>>>
            + AsStorage<Vertex<Geometry<B>>>
            + Consistent
            + Geometric,
    {
        let storage = storage.reborrow();
        let vertices = FaceView::bind(storage, abc)
            .ok_or_else(|| GraphError::TopologyNotFound)?
            .vertices()
            .map(|vertex| vertex.key())
            .collect();
        Ok(FacePokeCache {
            vertices,
            cache: FaceRemoveCache::snapshot(storage, abc)?,
        })
    }
}

pub struct FaceBridgeCache {
    source: SmallVec<[ArcKey; 4]>,
    destination: SmallVec<[ArcKey; 4]>,
    cache: (FaceRemoveCache, FaceRemoveCache),
}

impl FaceBridgeCache {
    pub fn snapshot<B>(
        storage: B,
        source: FaceKey,
        destination: FaceKey,
    ) -> Result<Self, GraphError>
    where
        B: Reborrow,
        B::Target: AsStorage<Arc<Geometry<B>>>
            + AsStorage<Face<Geometry<B>>>
            + AsStorage<Vertex<Geometry<B>>>
            + Consistent
            + Geometric,
    {
        let storage = storage.reborrow();
        let cache = (
            FaceRemoveCache::snapshot(storage, source)?,
            FaceRemoveCache::snapshot(storage, destination)?,
        );
        // Ensure that the opposite face exists and has the same arity.
        let source = FaceView::bind(storage, source).ok_or_else(|| GraphError::TopologyNotFound)?;
        let destination =
            FaceView::bind(storage, destination).ok_or_else(|| GraphError::TopologyNotFound)?;
        if source.arity() != destination.arity() {
            return Err(GraphError::ArityNonUniform);
        }
        Ok(FaceBridgeCache {
            source: source.interior_arcs().map(|arc| arc.key()).collect(),
            destination: destination.interior_arcs().map(|arc| arc.key()).collect(),
            cache,
        })
    }
}

pub struct FaceExtrudeCache {
    sources: Vec<VertexKey>,
    //destinations: Vec<G::Vertex>,
    //geometry: G::Face,
    cache: FaceRemoveCache,
}

impl FaceExtrudeCache {
    pub fn snapshot<B>(storage: B, abc: FaceKey) -> Result<Self, GraphError>
    where
        B: Reborrow,
        B::Target: AsStorage<Arc<Geometry<B>>>
            + AsStorage<Face<Geometry<B>>>
            + AsStorage<Vertex<Geometry<B>>>
            + Consistent
            + Geometric,
    {
        let storage = storage.reborrow();
        let cache = FaceRemoveCache::snapshot(storage, abc)?;
        let face = FaceView::bind(storage, abc).ok_or_else(|| GraphError::TopologyNotFound)?;
        let sources = face.vertices().keys().collect();
        Ok(FaceExtrudeCache { sources, cache })
    }
}

// TODO: Should this accept arc geometry at all?
pub fn insert_with<M, N, F>(
    mut mutation: N,
    cache: FaceInsertCache,
    f: F,
) -> Result<FaceKey, GraphError>
where
    N: AsMut<Mutation<M>>,
    M: Mutable,
    F: FnOnce() -> (
        <Geometry<M> as GraphGeometry>::Arc,
        <Geometry<M> as GraphGeometry>::Face,
    ),
{
    let FaceInsertCache {
        perimeter,
        connectivity,
    } = cache;
    let geometry = f();
    // Insert edges and collect the interior arcs.
    let arcs = perimeter
        .iter()
        .cloned()
        .perimeter()
        .map(|(a, b)| {
            edge::get_or_insert_with(mutation.as_mut(), (a, b), || {
                (Default::default(), geometry.0)
            })
            .map(|(_, (ab, _))| ab)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Insert the face.
    let face = mutation
        .as_mut()
        .storage
        .insert(Face::new(arcs[0], geometry.1));
    mutation.as_mut().connect_face_interior(&arcs, face)?;
    mutation
        .as_mut()
        .connect_face_exterior(&arcs, connectivity)?;
    Ok(face)
}

// TODO: Does this require a cache (or consistency)?
// TODO: This may need to be more destructive to maintain consistency. Edges,
//       arcs, and vertices may also need to be removed.
pub fn remove<M, N>(
    mut mutation: N,
    cache: FaceRemoveCache,
) -> Result<Face<Geometry<M>>, GraphError>
where
    N: AsMut<Mutation<M>>,
    M: Mutable,
{
    let FaceRemoveCache { abc, arcs } = cache;
    mutation.as_mut().disconnect_face_interior(&arcs)?;
    let face = mutation
        .as_mut()
        .storage
        .remove(&abc)
        .ok_or_else(|| GraphError::TopologyNotFound)?;
    Ok(face)
}

pub fn split<M, N>(mut mutation: N, cache: FaceSplitCache) -> Result<ArcKey, GraphError>
where
    N: AsMut<Mutation<M>>,
    M: Mutable,
{
    let FaceSplitCache { cache, left, right } = cache;
    remove(mutation.as_mut(), cache)?;
    let ab = (left[0], right[0]).into();
    let left = FaceInsertCache::snapshot(mutation.as_mut(), left)?;
    let right = FaceInsertCache::snapshot(mutation.as_mut(), right)?;
    insert_with(mutation.as_mut(), left, Default::default)?;
    insert_with(mutation.as_mut(), right, Default::default)?;
    Ok(ab)
}

pub fn poke_with<M, N, F>(
    mut mutation: N,
    cache: FacePokeCache,
    f: F,
) -> Result<VertexKey, GraphError>
where
    N: AsMut<Mutation<M>>,
    M: Mutable,
    F: FnOnce() -> <Geometry<M> as GraphGeometry>::Vertex,
{
    let FacePokeCache { vertices, cache } = cache;
    let face = remove(mutation.as_mut(), cache)?;
    let c = vertex::insert(mutation.as_mut(), f());
    for (a, b) in vertices.into_iter().perimeter() {
        let cache = FaceInsertCache::snapshot(mutation.as_mut(), &[a, b, c])?;
        insert_with(mutation.as_mut(), cache, || {
            (Default::default(), face.geometry)
        })?;
    }
    Ok(c)
}

pub fn bridge<M, N>(mut mutation: N, cache: FaceBridgeCache) -> Result<(), GraphError>
where
    N: AsMut<Mutation<M>>,
    M: Mutable,
{
    let FaceBridgeCache {
        source,
        destination,
        cache,
    } = cache;
    // Remove the source and destination faces. Pair the topology with edge
    // geometry for the source face.
    remove(mutation.as_mut(), cache.0)?;
    remove(mutation.as_mut(), cache.1)?;
    // TODO: Is it always correct to reverse the order of the opposite face's
    //       arcs?
    // Re-insert the arcs of the faces and bridge the mutual arcs.
    for (ab, cd) in source.into_iter().zip(destination.into_iter().rev()) {
        let cache = ArcBridgeCache::snapshot(mutation.as_mut(), ab, cd)?;
        edge::bridge(mutation.as_mut(), cache)?;
    }
    // TODO: Is there any reasonable entity this can return?
    Ok(())
}

pub fn extrude_with<M, N, F>(
    mut mutation: N,
    cache: FaceExtrudeCache,
    f: F,
) -> Result<FaceKey, GraphError>
where
    N: AsMut<Mutation<M>>,
    M: Mutable,
    F: Fn(<Geometry<M> as GraphGeometry>::Vertex) -> <Geometry<M> as GraphGeometry>::Vertex,
{
    let FaceExtrudeCache { sources, cache } = cache;
    remove(mutation.as_mut(), cache)?;
    let destinations = {
        let mutation = &*mutation.as_mut();
        sources
            .iter()
            .cloned()
            .flat_map(|a| VertexView::bind(mutation, a))
            .map(|source| f(source.geometry))
            .collect::<Vec<_>>()
    };
    if sources.len() != destinations.len() {
        return Err(GraphError::TopologyNotFound);
    }
    let destinations = destinations
        .into_iter()
        .map(|geometry| vertex::insert(mutation.as_mut(), geometry))
        .collect::<Vec<_>>();
    // Use the keys for the existing vertices and the translated geometries to
    // construct the extruded face and its connective faces.
    let cache = FaceInsertCache::snapshot(mutation.as_mut(), &destinations)?;
    let extrusion = insert_with(mutation.as_mut(), cache, Default::default)?;
    for ((a, c), (b, d)) in sources
        .into_iter()
        .zip(destinations.into_iter())
        .perimeter()
    {
        let cache = FaceInsertCache::snapshot(mutation.as_mut(), &[a, b, d, c])?;
        // TODO: Split these faces to form triangles.
        insert_with(mutation.as_mut(), cache, Default::default)?;
    }
    Ok(extrusion)
}

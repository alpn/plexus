use fool::BoolExt;
use std::borrow::Borrow;
use std::collections::{HashSet, VecDeque};

use crate::graph::edge::{Arc, ArcKey, ArcView};
use crate::graph::face::Ring;
use crate::graph::geometry::{Geometric, Geometry, GraphGeometry};
use crate::graph::mutation::Consistent;
use crate::graph::vertex::{Vertex, VertexKey, VertexView};
use crate::graph::{GraphError, OptionExt as _, Selector};
use crate::network::borrow::Reborrow;
use crate::network::storage::{AsStorage, AsStorageMut};
use crate::network::view::{ClosedView, View};
use crate::IteratorExt as _;

/// View of a path in a graph.
///
/// Provides a representation of non-intersecting paths in a graph. A path is
/// conceptually an ordered set of vertices that are joined by arcs. Paths are
/// notated as either sequences or sets. An open path over vertices $A$, $B$,
/// and $C$ is notated $\overrightarrow{(A,B,C)}$ and a closed path over the
/// same vertices is notated $\overrightarrow{\\{A,B,C\\}}$.
///
/// `Path` represents paths of the form $\overrightarrow{(A,\cdots,B)}$, where
/// $A$ is the back of the path and $B$ is the front of the path. Note that
/// closed paths are always of the form $\overrightarrow{(A,\cdots,A)}$, where
/// the back and front vertices are both $A$ (the same).
#[derive(Clone)]
pub struct Path<B>
where
    B: Reborrow,
    B::Target:
        AsStorage<Arc<Geometry<B>>> + AsStorage<Vertex<Geometry<B>>> + Consistent + Geometric,
{
    keys: VecDeque<ArcKey>,
    storage: B,
}

impl<B, M, G> Path<B>
where
    B: Reborrow<Target = M>,
    M: AsStorage<Arc<G>> + AsStorage<Vertex<G>> + Consistent + Geometric<Geometry = G>,
    G: GraphGeometry,
{
    // Paths bind multiple keys to storage and so do not support `View`,
    // `Orphan`, nor `ClosedView`. This bespoke `bind` function ensures that the
    // path is not empty and that the topology forms a non-intersecting path.
    pub(in crate::graph) fn bind<I>(storage: B, keys: I) -> Result<Self, GraphError>
    where
        I: IntoIterator,
        I::Item: Borrow<VertexKey>,
    {
        let mut keys = keys.into_iter().map(|key| *key.borrow());
        let a = keys.next().ok_or_else(|| GraphError::TopologyMalformed)?;
        let b = keys.next().ok_or_else(|| GraphError::TopologyMalformed)?;
        let ab = (a, b).into();
        View::<_, Arc<G>>::bind(storage.reborrow(), ab)
            .ok_or_else(|| GraphError::TopologyNotFound)?;
        let mut path = Path {
            keys: (&[ab]).iter().cloned().collect(),
            storage,
        };
        for key in keys {
            path.push_front(Selector::ByKey(key))?;
        }
        Ok(path)
    }

    /// Pushes a vertex onto the back of the path.
    ///
    /// The back of a path $\overrightarrow{(A,\cdots)}$ is the vertex $A$.
    /// This is the source vertex of the first arc that forms the path.
    ///
    /// The given vertex must be a source vertex of an arc formed with the the
    /// back of the path. That is, if the given vertex is $X$, then
    /// $\overrightarrow{XA}$ must exist.
    ///
    /// Returns the key of the arc $\overrightarrow{XA}$ inserted into the path
    /// using the given source vertex $X$.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is closed, the given vertex is not found,
    /// or the given vertex does not form an arc with the back of the path.
    pub fn push_back(&mut self, destination: Selector<VertexKey>) -> Result<ArcKey, GraphError> {
        self.is_open()
            .ok_or_else(|| GraphError::TopologyMalformed)?;
        let back = self.back();
        let xa = match destination {
            Selector::ByKey(key) => back
                .incoming_arcs()
                .find(|arc| arc.into_source_vertex().key() == key)
                .ok_or_else(|| GraphError::TopologyMalformed)?
                .key(),
            Selector::ByIndex(index) => {
                let x = back
                    .neighboring_vertices()
                    .keys()
                    .nth(index)
                    .ok_or_else(|| GraphError::TopologyNotFound)?;
                (x, back.key()).into()
            }
        };
        let (x, _) = xa.into();
        // Do not allow intersections unless they form a loop with the first
        // vertex in the path (this iteration skips the vertex at the front of
        // the path).
        let is_intersecting = self
            .arcs()
            .map(|arc| arc.into_source_vertex())
            .keys()
            .any(|key| key == x);
        if is_intersecting {
            Err(GraphError::TopologyMalformed)
        }
        else {
            self.keys.push_back(xa);
            Ok(xa)
        }
    }

    /// Pops a vertex from the back of the path.
    pub fn pop_back(&mut self) -> Option<ArcKey> {
        // Empty paths are forbidden.
        if self.keys.len() > 1 {
            self.keys.pop_back()
        }
        else {
            None
        }
    }

    /// Pushes a vertex onto the front of the path.
    ///
    /// The front of a path $\overrightarrow{(\cdots,B)}$ is the vertex $B$.
    /// This is the destination vertex of the last arc that forms the path.
    ///
    /// The given vertex must be a destination vertex of an arc formed with the
    /// the front of the path. That is, if the given vertex is $X$, then
    /// $\overrightarrow{BX}$ must exist.
    ///
    /// Returns the key of the arc $\overrightarrow{BX}$ inserted into the path
    /// using the given source vertex $X$.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is closed, the given vertex is not found,
    /// or the given vertex does not form an arc with the front of the path.
    pub fn push_front(&mut self, destination: Selector<VertexKey>) -> Result<ArcKey, GraphError> {
        self.is_open()
            .ok_or_else(|| GraphError::TopologyMalformed)?;
        let front = self.front();
        let bx = match destination {
            Selector::ByKey(key) => front
                .outgoing_arcs()
                .find(|arc| arc.into_destination_vertex().key() == key)
                .ok_or_else(|| GraphError::TopologyMalformed)?
                .key(),
            Selector::ByIndex(index) => {
                let x = front
                    .neighboring_vertices()
                    .keys()
                    .nth(index)
                    .ok_or_else(|| GraphError::TopologyNotFound)?;
                (front.key(), x).into()
            }
        };
        let (_, x) = bx.into();
        // Do not allow intersections unless they form a loop with the first
        // vertex in the path (this iteration skips the vertex at the back of
        // the path).
        let is_intersecting = self
            .arcs()
            .map(|arc| arc.into_destination_vertex())
            .keys()
            .any(|key| key == x);
        if is_intersecting {
            Err(GraphError::TopologyMalformed)
        }
        else {
            self.keys.push_front(bx);
            Ok(bx)
        }
    }

    /// Pops a vertex from the front of the path.
    pub fn pop_front(&mut self) -> Option<ArcKey> {
        // Empty paths are forbidden.
        if self.keys.len() > 1 {
            self.keys.pop_front()
        }
        else {
            None
        }
    }

    /// Gets the vertex at the back of the path.
    pub fn back(&self) -> VertexView<&M> {
        let (key, _) = self.endpoints();
        View::bind_into(self.storage.reborrow(), key).expect_consistent()
    }

    /// Gets the vertex at the front of the path.
    pub fn front(&self) -> VertexView<&M> {
        let (_, key) = self.endpoints();
        View::bind_into(self.storage.reborrow(), key).expect_consistent()
    }

    /// Converts the path into the ring it bisects, if any.
    ///
    /// A path bisects a ring if it is open, is not a boundary path, and its
    /// endpoint vertices participate in the ring.
    ///
    /// Returns the ring bisected by the path if such a ring exists, otherwise
    /// `None`.
    pub fn into_bisected_ring(self) -> Option<Ring<B>> {
        // The path may bisect a ring if it is open and is not a boundary path.
        // Note that open boundary paths cannot bisect a ring, so such paths are
        // ignored.
        if self.is_open() && !self.is_boundary_path() {
            let back = self.back();
            let front = self.front();
            // Get the ring of the first boundary arc of the vertex at the back
            // of the path.
            let ring = back
                .outgoing_arcs()
                .flat_map(|arc| arc.into_boundary_arc())
                .map(|arc| arc.into_ring())
                .nth(0);
            // If there is such a ring and it also includes the vertex at the
            // front of the path, then it is bisected. Rebind the path's storage
            // into a `Ring`.
            ring.filter(|ring| ring.vertices().keys().any(|key| key == front.key()))
                .map(|ring| ring.into_arc().key())
                .map(move |key| {
                    let Path { storage, .. } = self;
                    ArcView::from(View::bind_unchecked(storage, key)).into_ring()
                })
        }
        else {
            None
        }
    }

    /// Gets the ring bisected by the path, if any.
    pub fn bisected_ring(&self) -> Option<Ring<&M>> {
        self.interior_reborrow().into_bisected_ring()
    }

    /// Gets an iterator over the vertices in the path.
    pub fn vertices<'a>(&'a self) -> impl Iterator<Item = VertexView<&'a M>>
    where
        M: 'a,
    {
        let back = self.back();
        Some(back)
            .into_iter()
            .chain(self.arcs().map(|arc| arc.into_destination_vertex()))
    }

    /// Gets an iterator over the arcs in the path.
    pub fn arcs<'a>(&'a self) -> impl ExactSizeIterator<Item = ArcView<&'a M>>
    where
        M: 'a,
    {
        let storage = self.storage.reborrow();
        self.keys
            .iter()
            .rev()
            .cloned()
            .map(move |key| View::bind_into(storage, key).expect_consistent())
    }

    /// Returns `true` if the path is open.
    ///
    /// An _open path_ is a path that terminates and does **not** form a loop.
    pub fn is_open(&self) -> bool {
        !self.is_closed()
    }

    /// Returns `true` if the path is closed.
    ///
    /// A _closed path_ is a path that forms a loop by starting and ending at
    /// the same vertex.
    pub fn is_closed(&self) -> bool {
        let (a, b) = self.endpoints();
        a == b
    }

    /// Returns `true` if the path is a boundary path.
    ///
    /// A _boundary path_ is a path where all arcs forming the path are boundary
    /// arcs.
    pub fn is_boundary_path(&self) -> bool {
        !self.arcs().any(|arc| !arc.is_boundary_arc())
    }

    /// Returns `true` if the path is a bisecting path.
    ///
    /// A _bisecting path_ is a path that bisects a surface in a graph. A closed
    /// path is always a bisecting path, but open paths are not necessarily
    /// bisecting. See `into_bisected_ring`.
    pub fn is_bisecting_path(&self) -> bool {
        self.is_closed() || self.bisected_ring().is_some()
    }

    fn bind_unchecked<I>(storage: B, keys: I) -> Self
    where
        I: IntoIterator,
        I::Item: Borrow<ArcKey>,
    {
        let keys = keys.into_iter().map(|key| *key.borrow()).collect();
        Path { storage, keys }
    }

    fn endpoints(&self) -> (VertexKey, VertexKey) {
        let (a, _) = self.keys.back().cloned().expect("empty path").into();
        let (_, b) = self.keys.front().cloned().expect("empty pathy").into();
        (a, b)
    }

    fn interior_reborrow(&self) -> Path<&M> {
        let storage = self.storage.reborrow();
        let keys = self.keys.iter();
        Path::bind_unchecked(storage, keys)
    }
}

impl<'a, M, G> Path<&'a mut M>
where
    M: AsStorageMut<Arc<G>> + AsStorageMut<Vertex<G>> + Consistent + Geometric<Geometry = G>,
    G: GraphGeometry,
{
    /// Converts a mutable view into an immutable view.
    ///
    /// This is useful when mutations are not (or no longer) needed and mutual
    /// access is desired.
    pub fn into_ref(self) -> Path<&'a M> {
        let Path { keys, storage, .. } = self;
        Path {
            keys,
            storage: &*storage,
        }
    }
}

impl<B, M, G> PartialEq for Path<B>
where
    B: Reborrow<Target = M>,
    M: AsStorage<Arc<G>> + AsStorage<Vertex<G>> + Consistent + Geometric<Geometry = G>,
    G: GraphGeometry,
{
    fn eq(&self, other: &Self) -> bool {
        let keys = |path: &Self| path.arcs().keys().collect::<HashSet<_>>();
        keys(self) == keys(other)
    }
}

#[cfg(test)]
mod tests {
    use nalgebra::Point2;

    use crate::buffer::FromRawBuffers;
    use crate::graph::{ClosedView, MeshGraph, Selector};
    use crate::primitive::Trigon;
    use crate::IteratorExt;

    use Selector::ByKey;

    type E2 = Point2<f64>;

    #[test]
    fn open_close() {
        let graph = MeshGraph::<E2>::from_raw_buffers(
            vec![Trigon::from([0usize, 1, 2])],
            vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        )
        .unwrap();
        let keys = graph
            .faces()
            .nth(0)
            .unwrap()
            .interior_arcs()
            .map(|arc| arc.into_source_vertex())
            .keys()
            .collect::<Vec<_>>();

        let mut path = graph.path(keys.iter()).unwrap();
        assert!(path.is_open());
        // TODO: Move this assertion to a distinct test.
        assert_eq!(path.vertices().keys().collect::<Vec<_>>(), keys.to_vec());

        path.push_front(ByKey(keys[0])).unwrap();
        assert!(path.is_closed());
        assert_eq!(path.front().key(), path.back().key());
    }
}
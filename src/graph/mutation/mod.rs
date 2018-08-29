pub mod edge;
pub mod face;
mod region;
pub mod vertex;

use failure::Error;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::mem;
use std::ops::{Deref, DerefMut};

use geometry::Geometry;
use graph::container::alias::OwnedCore;
use graph::container::{Consistent, Container, Indeterminate};
use graph::mutation::face::FaceMutation;
use graph::storage::convert::AsStorage;
use graph::storage::Storage;
use graph::topology::{Edge, Face, Vertex};

pub trait Mutate: Sized {
    type Mutant;
    type Error: Debug;

    fn mutate(mutant: Self::Mutant) -> Self;

    fn commit(self) -> Result<Self::Mutant, Self::Error>;

    fn commit_with<F, T, E>(mut self, f: F) -> Result<(Self::Mutant, T), Self::Error>
    where
        F: FnOnce(&mut Self) -> Result<T, E>,
        E: Into<Self::Error>,
    {
        let output = f(&mut self);
        match output {
            Ok(value) => self.commit().map(|mutant| (mutant, value)),
            Err(error) => {
                self.abort();
                Err(error.into())
            }
        }
    }

    fn abort(self) {}
}

pub struct Replace<'a, M, N, G>
where
    M: 'a + Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    N: Mutate<Mutant = M>,
    G: 'a + Geometry,
{
    mutation: Option<(&'a mut M, N)>,
    phantom: PhantomData<G>,
}

impl<'a, M, N, G> Replace<'a, M, N, G>
where
    M: 'a + Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    N: Mutate<Mutant = M>,
    G: 'a + Geometry,
{
    pub fn replace(container: <Self as Mutate>::Mutant, replacement: M) -> Self {
        let mutant = mem::replace(container, replacement);
        Replace {
            mutation: Some((container, N::mutate(mutant))),
            phantom: PhantomData,
        }
    }

    fn drain(&mut self) -> (&'a mut M, N) {
        self.mutation.take().unwrap()
    }

    fn drain_and_commit(&mut self) -> Result<<Self as Mutate>::Mutant, <Self as Mutate>::Error> {
        let (container, mutation) = self.drain();
        let mutant = mutation.commit()?;
        mem::replace(container, mutant);
        Ok(container)
    }

    fn drain_and_abort(&mut self) {
        let (_, mutation) = self.drain();
        mutation.abort();
    }
}

impl<'a, M, G> AsRef<Mutation<M, G>> for Replace<'a, M, Mutation<M, G>, G>
where
    M: Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_ref(&self) -> &Mutation<M, G> {
        &self.mutation.as_ref().unwrap().1
    }
}

impl<'a, M, G> AsMut<Mutation<M, G>> for Replace<'a, M, Mutation<M, G>, G>
where
    M: Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_mut(&mut self) -> &mut Mutation<M, G> {
        &mut self.mutation.as_mut().unwrap().1
    }
}

impl<'a, M, N, G> Deref for Replace<'a, M, N, G>
where
    M: 'a + Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    N: Mutate<Mutant = M>,
    G: 'a + Geometry,
{
    type Target = N;

    fn deref(&self) -> &Self::Target {
        &self.mutation.as_ref().unwrap().1
    }
}

impl<'a, M, N, G> DerefMut for Replace<'a, M, N, G>
where
    M: 'a + Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    N: Mutate<Mutant = M>,
    G: 'a + Geometry,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.mutation.as_mut().unwrap().1
    }
}

impl<'a, M, N, G> Drop for Replace<'a, M, N, G>
where
    M: 'a + Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    N: Mutate<Mutant = M>,
    G: 'a + Geometry,
{
    fn drop(&mut self) {
        self.drain_and_abort();
    }
}

impl<'a, M, N, G> Mutate for Replace<'a, M, N, G>
where
    M: 'a + Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    N: Mutate<Mutant = M>,
    G: 'a + Geometry,
{
    type Mutant = &'a mut M;
    type Error = <N as Mutate>::Error;

    fn mutate(mutant: Self::Mutant) -> Self {
        Self::replace(mutant, M::default())
    }

    fn commit(mut self) -> Result<<Self as Mutate>::Mutant, Self::Error> {
        let mutant = self.drain_and_commit();
        mem::forget(self);
        mutant
    }

    fn abort(mut self) {
        self.drain_and_abort();
        mem::forget(self);
    }
}

/// Mesh mutation.
pub struct Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    mutation: FaceMutation<G>,
    phantom: PhantomData<M>,
}

impl<M, G> Mutation<M, G>
where
    M: Container<Contract = Consistent> + Default + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    pub fn replace(container: &mut M, replacement: M) -> Replace<M, Self, G> {
        Replace::replace(container, replacement)
    }
}

impl<M, G> AsRef<Self> for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_ref(&self) -> &Self {
        self
    }
}

impl<M, G> AsMut<Self> for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

impl<M, G> AsStorage<Edge<G>> for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_storage(&self) -> &Storage<Edge<G>> {
        self.mutation.as_edge_storage()
    }
}

impl<M, G> AsStorage<Face<G>> for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_storage(&self) -> &Storage<Face<G>> {
        self.mutation.as_face_storage()
    }
}

impl<M, G> AsStorage<Vertex<G>> for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn as_storage(&self) -> &Storage<Vertex<G>> {
        self.mutation.as_vertex_storage()
    }
}

impl<M, G> Container for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    type Contract = Indeterminate;
}

impl<M, G> Deref for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    type Target = FaceMutation<G>;

    fn deref(&self) -> &Self::Target {
        &self.mutation
    }
}

impl<M, G> DerefMut for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.mutation
    }
}

impl<M, G> Mutate for Mutation<M, G>
where
    M: Container<Contract = Consistent> + From<OwnedCore<G>> + Into<OwnedCore<G>>,
    G: Geometry,
{
    type Mutant = M;
    type Error = Error;

    fn mutate(container: Self::Mutant) -> Self {
        Mutation {
            mutation: FaceMutation::mutate(container.into()),
            phantom: PhantomData,
        }
    }

    fn commit(self) -> Result<Self::Mutant, Self::Error> {
        self.mutation.commit().map(|core| core.into())
    }
}
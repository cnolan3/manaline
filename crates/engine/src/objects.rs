//! The object arena. Objects are never removed (an object whose owner leaves
//! the game moves to `Zone::OutOfGame`), so a stable index is a stable
//! identity for the life of the game, and ids stay small enough to read as `#12`.

use crate::game::GameObject;
use crate::types::ObjectId;
use serde::{Deserialize, Serialize};
use std::ops::{Index, IndexMut};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Objects {
    items: Vec<GameObject>,
}

impl Objects {
    pub fn new() -> Objects {
        Objects::default()
    }

    /// Allocate the next id and build the object with it. Ids start at 1.
    pub fn insert_with_key(&mut self, f: impl FnOnce(ObjectId) -> GameObject) -> ObjectId {
        let id = ObjectId(self.items.len() as u32 + 1);
        self.items.push(f(id));
        id
    }

    fn slot(id: ObjectId) -> Option<usize> {
        (id.0 as usize).checked_sub(1)
    }

    pub fn get(&self, id: ObjectId) -> Option<&GameObject> {
        Self::slot(id).and_then(|i| self.items.get(i))
    }

    pub fn get_mut(&mut self, id: ObjectId) -> Option<&mut GameObject> {
        Self::slot(id).and_then(move |i| self.items.get_mut(i))
    }

    pub fn contains(&self, id: ObjectId) -> bool {
        self.get(id).is_some()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ObjectId, &GameObject)> {
        self.items.iter().enumerate().map(|(i, o)| (ObjectId(i as u32 + 1), o))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (ObjectId, &mut GameObject)> {
        self.items.iter_mut().enumerate().map(|(i, o)| (ObjectId(i as u32 + 1), o))
    }
}

impl Index<ObjectId> for Objects {
    type Output = GameObject;
    fn index(&self, id: ObjectId) -> &GameObject {
        self.get(id).unwrap_or_else(|| panic!("no object {id}"))
    }
}

impl IndexMut<ObjectId> for Objects {
    fn index_mut(&mut self, id: ObjectId) -> &mut GameObject {
        self.get_mut(id).unwrap_or_else(|| panic!("no object {id}"))
    }
}

impl<'a> IntoIterator for &'a Objects {
    type Item = (ObjectId, &'a GameObject);
    type IntoIter = Box<dyn Iterator<Item = (ObjectId, &'a GameObject)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

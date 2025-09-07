use std::collections::VecDeque;

pub struct SparseList<T> {
    data: Vec<Option<T>>,
    free_indices: VecDeque<usize>,
}

impl<T> SparseList<T> {
    pub fn new() -> Self {
        SparseList {
            data: Vec::new(),
            free_indices: VecDeque::new(),
        }
    }

    pub fn insert(&mut self, value: T) -> usize {
        if let Some(index) = self.free_indices.pop_front() {
            self.data[index] = Some(value);
            index
        } else {
            self.data.push(Some(value));
            self.data.len() - 1
        }
    }

    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index < self.data.len() {
            let ret = self.data[index].take();
            self.free_indices.push_back(index);
            ret
        } else {
            panic!("Index out of bounds");
        }
    }

    pub fn clear(&mut self) {
        self.data.iter_mut().for_each(|item| *item = None);
        self.free_indices.clear();
        self.free_indices.extend(0..self.data.len());
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index < self.data.len() {
            self.data[index].as_ref()
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index < self.data.len() {
            self.data[index].as_mut()
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
}

use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
};

pub(crate) struct Cache<K, V> {
    data: HashMap<K, V>,
    order: VecDeque<K>,
    size: usize,
}

impl<K: Eq + Hash + Clone, V> Cache<K, V> {
    pub(crate) fn new(size: usize) -> Cache<K, V> {
        assert!(size > 0);
        Cache {
            data: HashMap::new(),
            order: VecDeque::new(),
            size,
        }
    }

    pub(crate) fn push(&mut self, key: K, value: V) -> Option<(K, V)> {
        if self.data.contains_key(&key) {
            self.data.insert(key.clone(), value);

            let pos = self
                .order
                .iter()
                .position(|x| *x == key)
                .expect("Key not in order");
            self.order.remove(pos);
            self.order.push_back(key);
        } else {
            self.order.push_back(key.clone());
            self.data.insert(key, value);
        }

        if self.order.len() > self.size {
            let popped_key = self.order.pop_front().expect("No items in cache");
            let popped_val = self
                .data
                .remove(&popped_key)
                .expect("Missing object in item cache");
            Some((popped_key, popped_val))
        } else {
            None
        }
    }

    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.data.get(key)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_single_entry() {
        let mut cache = Cache::new(1);
        assert_eq!(cache.push(1, 1), None);
        assert_eq!(cache.push(2, 2), Some((1, 1)));
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&2));
    }

    #[test]
    fn test_rollover() {
        let mut cache = Cache::new(5);
        assert_eq!(cache.push(1, 1), None);
        assert_eq!(cache.push(2, 2), None);
        assert_eq!(cache.push(3, 3), None);
        assert_eq!(cache.push(4, 4), None);
        assert_eq!(cache.push(5, 5), None);
        assert_eq!(cache.get(&1), Some(&1));
        assert_eq!(cache.get(&2), Some(&2));
        assert_eq!(cache.get(&3), Some(&3));
        assert_eq!(cache.get(&4), Some(&4));
        assert_eq!(cache.get(&5), Some(&5));

        assert_eq!(cache.push(6, 6), Some((1, 1)));
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&2));
        assert_eq!(cache.get(&3), Some(&3));
        assert_eq!(cache.get(&4), Some(&4));
        assert_eq!(cache.get(&5), Some(&5));
        assert_eq!(cache.get(&6), Some(&6));
    }

    #[test]
    fn test_duplicate() {
        let mut cache = Cache::new(2);
        assert_eq!(cache.push(1, 1), None);
        assert_eq!(cache.push(2, 2), None);

        // This should replace the value of 1 with 3
        assert_eq!(cache.push(1, 3), None);
        assert_eq!(cache.get(&1), Some(&3));
        // 2 should still be valid
        assert_eq!(cache.get(&2), Some(&2));
        // Pushing a new value should replace 2 since it was inserted before the latest insertion
        // of 1
        assert_eq!(cache.push(3, 3), Some((2, 2)));
    }
}

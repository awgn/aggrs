use std::collections::HashMap;


pub trait Merge {
    fn merge(&mut self, other: Self);
}

impl<K, V> Merge for HashMap<K, V>
    where K: std::hash::Hash + Eq + Clone,
          V: std::ops::AddAssign + Default + Copy
{
    fn merge(&mut self, other: Self) {
        for (k, v) in other {
            *self.entry(k.clone()).or_default() += v;
        }
    }
}
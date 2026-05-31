use ahash::RandomState;
use hashbrown::hash_map::RawEntryMut;
use hashbrown::HashMap;

// Alias: HashMap with ahash for faster hashing.
pub type AHashMap<K, V> = HashMap<K, V, RandomState>;

pub trait Merge {
    fn merge(&mut self, other: &Self);
}

impl<K, V, S> Merge for HashMap<K, V, S>
where
    K: std::hash::Hash + Eq + Clone,
    V: std::ops::AddAssign + Default + Copy,
    S: std::hash::BuildHasher,
{
    fn merge(&mut self, other: &Self) {
        for (k, v) in other {
            match self.raw_entry_mut().from_key(k) {
                RawEntryMut::Occupied(mut entry) => {
                    *entry.get_mut() += *v;
                }
                RawEntryMut::Vacant(entry) => {
                    let mut val = V::default();
                    val += *v;
                    entry.insert(k.clone(), val);
                }
            };
        }
    }
}

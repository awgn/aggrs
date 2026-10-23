use ahash::RandomState;
use hashbrown::HashMap;

// Alias: HashMap with ahash for faster hashing.
pub type AHashMap<K, V> = HashMap<K, V, RandomState>;

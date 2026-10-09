use super::{ObjectId, ObjectVersion, SubjectId, VirtualMachine, VmError, invalid_state};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use tf_format::Program;

const DECODED_PROGRAM_CACHE_CAPACITY: usize = 256;
const COMPILED_SOURCE_CACHE_CAPACITY: usize = 128;
const COMPILATION_SEMANTICS_VERSION: &[u8] = b"praxis-otf-semantics-v1";

#[derive(Debug, Default)]
pub(super) struct ProgramCache {
    entries: BTreeMap<ObjectId, (ObjectVersion, Arc<Program>)>,
    lru: VecDeque<ObjectId>,
}

impl ProgramCache {
    pub(super) fn get(&mut self, object: ObjectId, version: ObjectVersion) -> Option<Arc<Program>> {
        let cached = self.entries.get(&object)?;
        if cached.0 != version {
            self.remove(object);
            return None;
        }
        let program = Arc::clone(&cached.1);
        self.touch(object);
        Some(program)
    }

    pub(super) fn insert(
        &mut self,
        object: ObjectId,
        version: ObjectVersion,
        program: Arc<Program>,
    ) {
        self.remove(object);
        self.entries.insert(object, (version, program));
        self.lru.push_back(object);
        while self.entries.len() > DECODED_PROGRAM_CACHE_CAPACITY {
            if let Some(oldest) = self.lru.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, object: ObjectId) {
        self.entries.remove(&object);
        self.lru.retain(|cached| *cached != object);
    }

    fn touch(&mut self, object: ObjectId) {
        self.lru.retain(|cached| *cached != object);
        self.lru.push_back(object);
    }
}

#[derive(Debug, Default)]
pub(super) struct CompilationCache {
    entries: BTreeMap<[u8; 32], Arc<Program>>,
    lru: VecDeque<[u8; 32]>,
    hits: u64,
    misses: u64,
}

impl CompilationCache {
    pub(super) fn get(&mut self, key: [u8; 32]) -> Option<Arc<Program>> {
        if let Some(program) = self.entries.get(&key).cloned() {
            self.hits = self.hits.saturating_add(1);
            self.lru.retain(|cached| *cached != key);
            self.lru.push_back(key);
            return Some(program);
        }
        self.misses = self.misses.saturating_add(1);
        None
    }

    pub(super) fn insert(&mut self, key: [u8; 32], compiled: Program) -> Arc<Program> {
        if let Some(program) = self.entries.get(&key).cloned() {
            // Concurrent callers may compile the same miss outside the cache
            // lock. Publish and return the first complete immutable Program.
            self.lru.retain(|cached| *cached != key);
            self.lru.push_back(key);
            return program;
        }
        let compiled = Arc::new(compiled);
        self.entries.insert(key, Arc::clone(&compiled));
        self.lru.push_back(key);
        while self.entries.len() > COMPILED_SOURCE_CACHE_CAPACITY {
            if let Some(oldest) = self.lru.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        compiled
    }

    #[cfg(test)]
    pub(super) const fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

#[derive(Clone, Copy)]
pub(super) enum CompilationMode {
    Program,
    Interactive,
}

pub(super) fn compilation_cache_key(
    mode: CompilationMode,
    subject: SubjectId,
    source: &str,
    expanded_source: &str,
    dependencies: &BTreeMap<ObjectId, String>,
) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"ousject-compilation-cache\0");
    append_field(&mut bytes, COMPILATION_SEMANTICS_VERSION);
    append_field(&mut bytes, praxis_compiler::COMPILER_VERSION.as_bytes());
    append_field(
        &mut bytes,
        match mode {
            CompilationMode::Program => b"program",
            CompilationMode::Interactive => b"interactive",
        },
    );
    append_field(&mut bytes, subject.to_string().as_bytes());
    append_field(&mut bytes, source.as_bytes());
    append_field(&mut bytes, expanded_source.as_bytes());
    for (identity, content_hash) in dependencies {
        append_field(&mut bytes, identity.to_string().as_bytes());
        append_field(&mut bytes, content_hash.as_bytes());
    }
    ousject_auth::sha256_digest(&bytes)
}

fn append_field(output: &mut Vec<u8>, field: &[u8]) {
    let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(field);
}

impl VirtualMachine {
    pub(super) fn compile_cached(
        &self,
        key: [u8; 32],
        compile: impl FnOnce() -> Result<Program, VmError>,
    ) -> Result<Arc<Program>, VmError> {
        if let Some(program) = self
            .compilation_cache
            .lock()
            .map_err(|_| invalid_state("Compilation cache unavailable"))?
            .get(key)
        {
            return Ok(program);
        }

        // Keep parsing and user-controlled Module loading outside the cache
        // mutex. A racing miss may compile twice; insertion publishes only one
        // immutable OTF Program.
        let compiled = compile()?;
        let mut cache = self
            .compilation_cache
            .lock()
            .map_err(|_| invalid_state("Compilation cache unavailable"))?;
        Ok(cache.insert(key, compiled))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::SYSTEM_SUBJECT;
    use core::cell::Cell;
    use oms_runtime::InMemoryObjectManager;
    use tf_format::Token;
    use tf_format::Value;

    #[test]
    fn compiled_source_cache_reuses_successful_otf_and_drops_oldest_entry() {
        let manager = Arc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = VirtualMachine::new(manager);
        let subject = SYSTEM_SUBJECT;
        let key = compilation_cache_key(
            CompilationMode::Interactive,
            subject,
            "counter++",
            "counter++\n",
            &BTreeMap::new(),
        );
        let compilations = Cell::new(0);
        let first = vm
            .compile_cached(key, || {
                compilations.set(compilations.get() + 1);
                Ok(Program {
                    tokens: vec![Token::Push(Value::Integer(1)), Token::Halt],
                })
            })
            .expect("compile cache miss");
        let second = vm
            .compile_cached(key, || {
                compilations.set(compilations.get() + 1);
                Ok(Program {
                    tokens: vec![Token::Push(Value::Integer(2)), Token::Halt],
                })
            })
            .expect("compile cache hit");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(compilations.get(), 1);
        assert_eq!(
            vm.compilation_cache
                .lock()
                .expect("lock compile cache")
                .stats(),
            (1, 1)
        );

        let mut cache = ProgramCache::default();
        let mut objects = Vec::new();
        for _ in 0..DECODED_PROGRAM_CACHE_CAPACITY {
            let object = ObjectId::new();
            objects.push(object);
            cache.insert(
                object,
                ObjectVersion::default(),
                Arc::new(Program {
                    tokens: vec![Token::Halt],
                }),
            );
        }
        assert!(cache.get(objects[0], ObjectVersion::default()).is_some());
        let newest = ObjectId::new();
        cache.insert(
            newest,
            ObjectVersion::default(),
            Arc::new(Program {
                tokens: vec![Token::Halt],
            }),
        );
        assert!(cache.get(objects[0], ObjectVersion::default()).is_some());
        assert!(cache.get(objects[1], ObjectVersion::default()).is_none());
        assert_eq!(cache.entries.len(), DECODED_PROGRAM_CACHE_CAPACITY);
        assert_eq!(cache.lru.len(), DECODED_PROGRAM_CACHE_CAPACITY);
    }

    #[test]
    fn compilation_key_frames_source_mode_subject_and_module_dependencies() {
        let source = "value = 1";
        let subject = SYSTEM_SUBJECT;
        let plain = compilation_cache_key(
            CompilationMode::Interactive,
            subject,
            source,
            source,
            &BTreeMap::new(),
        );
        assert_ne!(
            plain,
            compilation_cache_key(
                CompilationMode::Program,
                subject,
                source,
                source,
                &BTreeMap::new(),
            )
        );
        assert_ne!(
            plain,
            compilation_cache_key(
                CompilationMode::Interactive,
                SubjectId::new(),
                source,
                source,
                &BTreeMap::new(),
            )
        );
        assert_ne!(
            plain,
            compilation_cache_key(
                CompilationMode::Interactive,
                subject,
                "value = 12",
                "value = 12",
                &BTreeMap::new(),
            )
        );
        assert_ne!(
            plain,
            compilation_cache_key(
                CompilationMode::Interactive,
                subject,
                source,
                source,
                &BTreeMap::from([(ObjectId::new(), String::from("module-hash"))]),
            )
        );
    }
}

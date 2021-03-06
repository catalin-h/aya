//! Hash map types.
use std::{
    convert::TryFrom,
    marker::PhantomData,
    mem,
    ops::{Deref, DerefMut},
    os::unix::io::RawFd,
};

use crate::{
    generated::bpf_map_type::BPF_MAP_TYPE_HASH,
    maps::{IterableMap, Map, MapError, MapIter, MapKeys, MapRef, MapRefMut},
    sys::{bpf_map_delete_elem, bpf_map_lookup_elem, bpf_map_update_elem},
    Pod,
};

/// A hash map stored inside the kernel, in which both user-space and eBPF programs can insert and
/// lookup values from.
///
/// The types of the keys and values must be plain old data (POD), meaning that they
/// must be safely convertible to and from byte slices.
///
/// # Example
///
/// ```no_run
/// # let bpf = aya::Bpf::load(&[], None)?;
/// use aya::maps::HashMap;
/// use std::convert::TryFrom;
///
/// const CONFIG_KEY_NUM_RETRIES: u8 = 1;
///
/// let mut hm = HashMap::try_from(bpf.map_mut("CONFIG")?)?;
/// hm.insert(CONFIG_KEY_NUM_RETRIES, 3, 0 /* flags */);
/// # Ok::<(), aya::BpfError>(())
/// ```
pub struct HashMap<T: Deref<Target = Map>, K, V> {
    inner: T,
    _k: PhantomData<K>,
    _v: PhantomData<V>,
}

impl<T: Deref<Target = Map>, K: Pod, V: Pod> HashMap<T, K, V> {
    pub(crate) fn new(map: T) -> Result<HashMap<T, K, V>, MapError> {
        let map_type = map.obj.def.map_type;

        // validate the map definition
        if map_type != BPF_MAP_TYPE_HASH as u32 {
            return Err(MapError::InvalidMapType {
                map_type: map_type as u32,
            })?;
        }
        let size = mem::size_of::<K>();
        let expected = map.obj.def.key_size as usize;
        if size != expected {
            return Err(MapError::InvalidKeySize { size, expected });
        }

        let size = mem::size_of::<V>();
        let expected = map.obj.def.value_size as usize;
        if size != expected {
            return Err(MapError::InvalidValueSize { size, expected });
        }

        // make sure the map has been created
        let _fd = map.fd_or_err()?;

        Ok(HashMap {
            inner: map,
            _k: PhantomData,
            _v: PhantomData,
        })
    }

    /// Returns a copy of the value associated with the key.
    pub unsafe fn get(&self, key: &K, flags: u64) -> Result<Option<V>, MapError> {
        let fd = self.inner.deref().fd_or_err()?;
        bpf_map_lookup_elem(fd, key, flags).map_err(|(code, io_error)| MapError::SyscallError {
            call: "bpf_map_lookup_elem".to_owned(),
            code,
            io_error,
        })
    }

    /// An iterator visiting all key-value pairs in arbitrary order. The
    /// iterator item type is `Result<(K, V), MapError>`.
    pub unsafe fn iter<'coll>(&'coll self) -> MapIter<'coll, K, V> {
        MapIter::new(self)
    }

    /// An iterator visiting all keys in arbitrary order. The iterator element
    /// type is `Result<K, MapError>`.
    pub unsafe fn keys<'coll>(&'coll self) -> MapKeys<'coll, K, V> {
        MapKeys::new(self)
    }
}

impl<T: DerefMut<Target = Map>, K: Pod, V: Pod> HashMap<T, K, V> {
    /// Inserts a key-value pair into the map.
    pub fn insert(&mut self, key: K, value: V, flags: u64) -> Result<(), MapError> {
        let fd = self.inner.deref_mut().fd_or_err()?;
        bpf_map_update_elem(fd, &key, &value, flags).map_err(|(code, io_error)| {
            MapError::SyscallError {
                call: "bpf_map_update_elem".to_owned(),
                code,
                io_error,
            }
        })?;
        Ok(())
    }

    /// Removes a key from the map.
    pub fn remove(&mut self, key: &K) -> Result<(), MapError> {
        let fd = self.inner.deref_mut().fd_or_err()?;
        bpf_map_delete_elem(fd, key)
            .map(|_| ())
            .map_err(|(code, io_error)| MapError::SyscallError {
                call: "bpf_map_delete_elem".to_owned(),
                code,
                io_error,
            })
    }
}

impl<T: Deref<Target = Map>, K: Pod, V: Pod> IterableMap<K, V> for HashMap<T, K, V> {
    fn fd(&self) -> Result<RawFd, MapError> {
        self.inner.deref().fd_or_err()
    }

    unsafe fn get(&self, key: &K) -> Result<Option<V>, MapError> {
        HashMap::get(self, key, 0)
    }
}

impl<K: Pod, V: Pod> TryFrom<MapRef> for HashMap<MapRef, K, V> {
    type Error = MapError;

    fn try_from(a: MapRef) -> Result<HashMap<MapRef, K, V>, MapError> {
        HashMap::new(a)
    }
}

impl<K: Pod, V: Pod> TryFrom<MapRefMut> for HashMap<MapRefMut, K, V> {
    type Error = MapError;

    fn try_from(a: MapRefMut) -> Result<HashMap<MapRefMut, K, V>, MapError> {
        HashMap::new(a)
    }
}

impl<'a, K: Pod, V: Pod> TryFrom<&'a Map> for HashMap<&'a Map, K, V> {
    type Error = MapError;

    fn try_from(a: &'a Map) -> Result<HashMap<&'a Map, K, V>, MapError> {
        HashMap::new(a)
    }
}

impl<'a, K: Pod, V: Pod> TryFrom<&'a mut Map> for HashMap<&'a mut Map, K, V> {
    type Error = MapError;

    fn try_from(a: &'a mut Map) -> Result<HashMap<&'a mut Map, K, V>, MapError> {
        HashMap::new(a)
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use libc::{EFAULT, ENOENT};

    use crate::{
        bpf_map_def,
        generated::{
            bpf_attr, bpf_cmd,
            bpf_map_type::{BPF_MAP_TYPE_HASH, BPF_MAP_TYPE_PERF_EVENT_ARRAY},
        },
        obj,
        sys::{override_syscall, SysResult, Syscall},
    };

    use super::*;

    fn new_obj_map(name: &str) -> obj::Map {
        obj::Map {
            name: name.to_string(),
            def: bpf_map_def {
                map_type: BPF_MAP_TYPE_HASH as u32,
                key_size: 4,
                value_size: 4,
                max_entries: 1024,
                map_flags: 0,
            },
            section_index: 0,
            data: Vec::new(),
        }
    }

    fn sys_error(value: i32) -> SysResult {
        Err((-1, io::Error::from_raw_os_error(value)))
    }

    #[test]
    fn test_wrong_key_size() {
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: None,
        };
        assert!(matches!(
            HashMap::<_, u8, u32>::new(&map),
            Err(MapError::InvalidKeySize {
                size: 1,
                expected: 4
            })
        ));
    }

    #[test]
    fn test_wrong_value_size() {
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: None,
        };
        assert!(matches!(
            HashMap::<_, u32, u16>::new(&map),
            Err(MapError::InvalidValueSize {
                size: 2,
                expected: 4
            })
        ));
    }

    #[test]
    fn test_try_from_wrong_map() {
        let map = Map {
            obj: obj::Map {
                name: "TEST".to_string(),
                def: bpf_map_def {
                    map_type: BPF_MAP_TYPE_PERF_EVENT_ARRAY as u32,
                    key_size: 4,
                    value_size: 4,
                    max_entries: 1024,
                    map_flags: 0,
                },
                section_index: 0,
                data: Vec::new(),
            },
            fd: None,
        };

        assert!(matches!(
            HashMap::<_, u32, u32>::try_from(&map),
            Err(MapError::InvalidMapType { .. })
        ));
    }

    #[test]
    fn test_new_not_created() {
        let mut map = Map {
            obj: new_obj_map("TEST"),
            fd: None,
        };

        assert!(matches!(
            HashMap::<_, u32, u32>::new(&mut map),
            Err(MapError::NotCreated { .. })
        ));
    }

    #[test]
    fn test_new_ok() {
        let mut map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };

        assert!(HashMap::<_, u32, u32>::new(&mut map).is_ok());
    }

    #[test]
    fn test_try_from_ok() {
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        assert!(HashMap::<_, u32, u32>::try_from(&map).is_ok())
    }

    #[test]
    fn test_insert_syscall_error() {
        override_syscall(|_| sys_error(EFAULT));

        let mut map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let mut hm = HashMap::<_, u32, u32>::new(&mut map).unwrap();

        assert!(matches!(
            hm.insert(1, 42, 0),
            Err(MapError::SyscallError { call, code: -1, io_error }) if call == "bpf_map_update_elem" && io_error.raw_os_error() == Some(EFAULT)
        ));
    }

    #[test]
    fn test_insert_ok() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_UPDATE_ELEM,
                ..
            } => Ok(1),
            _ => sys_error(EFAULT),
        });

        let mut map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let mut hm = HashMap::<_, u32, u32>::new(&mut map).unwrap();

        assert!(hm.insert(1, 42, 0).is_ok());
    }

    #[test]
    fn test_remove_syscall_error() {
        override_syscall(|_| sys_error(EFAULT));

        let mut map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let mut hm = HashMap::<_, u32, u32>::new(&mut map).unwrap();

        assert!(matches!(
            hm.remove(&1),
            Err(MapError::SyscallError { call, code: -1, io_error }) if call == "bpf_map_delete_elem" && io_error.raw_os_error() == Some(EFAULT)
        ));
    }

    #[test]
    fn test_remove_ok() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_DELETE_ELEM,
                ..
            } => Ok(1),
            _ => sys_error(EFAULT),
        });

        let mut map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let mut hm = HashMap::<_, u32, u32>::new(&mut map).unwrap();

        assert!(hm.remove(&1).is_ok());
    }

    #[test]
    fn test_get_syscall_error() {
        override_syscall(|_| sys_error(EFAULT));
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        assert!(matches!(
            unsafe { hm.get(&1, 0) },
            Err(MapError::SyscallError { call, code: -1, io_error }) if call == "bpf_map_lookup_elem" && io_error.raw_os_error() == Some(EFAULT)
        ));
    }

    #[test]
    fn test_get_not_found() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_LOOKUP_ELEM,
                ..
            } => sys_error(ENOENT),
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        assert!(matches!(unsafe { hm.get(&1, 0) }, Ok(None)));
    }

    fn bpf_key<T: Copy>(attr: &bpf_attr) -> Option<T> {
        match unsafe { attr.__bindgen_anon_2.key } as *const T {
            p if p.is_null() => None,
            p => Some(unsafe { *p }),
        }
    }

    fn set_next_key<T: Copy>(attr: &bpf_attr, next: T) {
        let key = unsafe { attr.__bindgen_anon_2.__bindgen_anon_1.next_key } as *const T as *mut T;
        unsafe { *key = next };
    }

    fn set_ret<T: Copy>(attr: &bpf_attr, ret: T) {
        let value = unsafe { attr.__bindgen_anon_2.__bindgen_anon_1.value } as *const T as *mut T;
        unsafe { *value = ret };
    }

    #[test]
    fn test_keys_empty() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                ..
            } => sys_error(ENOENT),
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();
        let keys = unsafe { hm.keys() }.collect::<Result<Vec<_>, _>>();
        assert!(matches!(keys, Ok(ks) if ks.is_empty()))
    }

    fn get_next_key(attr: &bpf_attr) -> SysResult {
        match bpf_key(&attr) {
            None => set_next_key(&attr, 10),
            Some(10) => set_next_key(&attr, 20),
            Some(20) => set_next_key(&attr, 30),
            Some(30) => return sys_error(ENOENT),
            Some(_) => return sys_error(EFAULT),
        };

        Ok(1)
    }

    fn lookup_elem(attr: &bpf_attr) -> SysResult {
        match bpf_key(&attr) {
            Some(10) => set_ret(&attr, 100),
            Some(20) => set_ret(&attr, 200),
            Some(30) => set_ret(&attr, 300),
            Some(_) => return sys_error(ENOENT),
            None => return sys_error(EFAULT),
        };

        Ok(1)
    }

    #[test]
    fn test_keys() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                attr,
            } => get_next_key(&attr),
            _ => sys_error(EFAULT),
        });

        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        let keys = unsafe { hm.keys() }.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(&keys, &[10, 20, 30])
    }

    #[test]
    fn test_keys_error() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                attr,
            } => {
                match bpf_key(&attr) {
                    None => set_next_key(&attr, 10),
                    Some(10) => set_next_key(&attr, 20),
                    Some(_) => return sys_error(EFAULT),
                };

                Ok(1)
            }
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        let mut keys = unsafe { hm.keys() };
        assert!(matches!(keys.next(), Some(Ok(10))));
        assert!(matches!(keys.next(), Some(Ok(20))));
        assert!(matches!(
            keys.next(),
            Some(Err(MapError::SyscallError { call, .. })) if call == "bpf_map_get_next_key"
        ));
        assert!(matches!(keys.next(), None));
    }

    #[test]
    fn test_iter() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                attr,
            } => get_next_key(&attr),
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_LOOKUP_ELEM,
                attr,
            } => lookup_elem(&attr),
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();
        let items = unsafe { hm.iter() }.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(&items, &[(10, 100), (20, 200), (30, 300)])
    }

    #[test]
    fn test_iter_key_deleted() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                attr,
            } => get_next_key(&attr),
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_LOOKUP_ELEM,
                attr,
            } => {
                match bpf_key(&attr) {
                    Some(10) => set_ret(&attr, 100),
                    Some(20) => return sys_error(ENOENT),
                    Some(30) => set_ret(&attr, 300),
                    Some(_) => return sys_error(ENOENT),
                    None => return sys_error(EFAULT),
                };

                Ok(1)
            }
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        let items = unsafe { hm.iter() }.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(&items, &[(10, 100), (30, 300)])
    }

    #[test]
    fn test_iter_key_error() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                attr,
            } => {
                match bpf_key(&attr) {
                    None => set_next_key(&attr, 10),
                    Some(10) => set_next_key(&attr, 20),
                    Some(20) => return sys_error(EFAULT),
                    Some(30) => return sys_error(ENOENT),
                    Some(_) => panic!(),
                };

                Ok(1)
            }
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_LOOKUP_ELEM,
                attr,
            } => lookup_elem(&attr),
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        let mut iter = unsafe { hm.iter() };
        assert!(matches!(iter.next(), Some(Ok((10, 100)))));
        assert!(matches!(iter.next(), Some(Ok((20, 200)))));
        assert!(matches!(
            iter.next(),
            Some(Err(MapError::SyscallError { call, .. })) if call == "bpf_map_get_next_key"
        ));
        assert!(matches!(iter.next(), None));
    }

    #[test]
    fn test_iter_value_error() {
        override_syscall(|call| match call {
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_GET_NEXT_KEY,
                attr,
            } => get_next_key(&attr),
            Syscall::Bpf {
                cmd: bpf_cmd::BPF_MAP_LOOKUP_ELEM,
                attr,
            } => {
                match bpf_key(&attr) {
                    Some(10) => set_ret(&attr, 100),
                    Some(20) => return sys_error(EFAULT),
                    Some(30) => set_ret(&attr, 300),
                    Some(_) => return sys_error(ENOENT),
                    None => return sys_error(EFAULT),
                };

                Ok(1)
            }
            _ => sys_error(EFAULT),
        });
        let map = Map {
            obj: new_obj_map("TEST"),
            fd: Some(42),
        };
        let hm = HashMap::<_, u32, u32>::new(&map).unwrap();

        let mut iter = unsafe { hm.iter() };
        assert!(matches!(iter.next(), Some(Ok((10, 100)))));
        assert!(matches!(
            iter.next(),
            Some(Err(MapError::SyscallError { call, .. })) if call == "bpf_map_lookup_elem"
        ));
        assert!(matches!(iter.next(), Some(Ok((30, 300)))));
        assert!(matches!(iter.next(), None));
    }
}

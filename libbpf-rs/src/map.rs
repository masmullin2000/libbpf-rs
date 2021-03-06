use core::ffi::c_void;
use std::convert::TryFrom;
use std::path::Path;

use bitflags::bitflags;
use nix::errno;
use num_enum::TryFromPrimitive;
use strum_macros::Display;

use crate::*;

/// Represents a parsed but not yet loaded BPF map.
///
/// This object exposes operations that need to happen before the map is created.
///
/// Some methods require working with raw bytes. You may find libraries such as
/// [`plain`](https://crates.io/crates/plain) helpful.
pub struct OpenMap {
    ptr: *mut libbpf_sys::bpf_map,
}

impl OpenMap {
    pub(crate) fn new(ptr: *mut libbpf_sys::bpf_map) -> Self {
        OpenMap { ptr }
    }

    pub fn set_map_ifindex(&mut self, idx: u32) {
        unsafe { libbpf_sys::bpf_map__set_ifindex(self.ptr, idx) };
    }

    pub fn set_initial_value(&mut self, data: &[u8]) -> Result<()> {
        let ret = unsafe {
            libbpf_sys::bpf_map__set_initial_value(
                self.ptr,
                data.as_ptr() as *const std::ffi::c_void,
                data.len() as libbpf_sys::size_t,
            )
        };

        if ret != 0 {
            // Error code is returned negative, flip to positive to match errno
            return Err(Error::System(-ret));
        }

        Ok(())
    }

    pub fn set_max_entries(&mut self, count: u32) -> Result<()> {
        let ret = unsafe { libbpf_sys::bpf_map__set_max_entries(self.ptr, count) };

        if ret != 0 {
            // Error code is returned negative, flip to positive to match errno
            return Err(Error::System(-ret));
        }

        Ok(())
    }

    pub fn set_inner_map_fd(&mut self, inner: &Map) {
        unsafe { libbpf_sys::bpf_map__set_inner_map_fd(self.ptr, inner.fd()) };
    }
}

/// Represents a created map.
///
/// Some methods require working with raw bytes. You may find libraries such as
/// [`plain`](https://crates.io/crates/plain) helpful.
pub struct Map {
    fd: i32,
    name: String,
    ty: libbpf_sys::bpf_map_type,
    key_size: u32,
    value_size: u32,
    ptr: *mut libbpf_sys::bpf_map,
}

impl Map {
    pub(crate) fn new(
        fd: i32,
        name: String,
        ty: libbpf_sys::bpf_map_type,
        key_size: u32,
        value_size: u32,
        ptr: *mut libbpf_sys::bpf_map,
    ) -> Self {
        Map {
            fd,
            name,
            ty,
            key_size,
            value_size,
            ptr,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns a file descriptor to the underlying map.
    pub fn fd(&self) -> i32 {
        self.fd
    }

    pub fn map_type(&self) -> MapType {
        match MapType::try_from(self.ty) {
            Ok(t) => t,
            Err(_) => MapType::Unknown,
        }
    }

    /// Key size in bytes
    pub fn key_size(&self) -> u32 {
        self.key_size
    }

    /// Value size in bytes
    pub fn value_size(&self) -> u32 {
        self.value_size
    }

    /// [Pin](https://facebookmicrosites.github.io/bpf/blog/2018/08/31/object-lifetime.html#bpffs)
    /// this map to bpffs.
    pub fn pin<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path_c = util::path_to_cstring(path)?;
        let path_ptr = path_c.as_ptr();

        let ret = unsafe { libbpf_sys::bpf_map__pin(self.ptr, path_ptr) };
        if ret != 0 {
            // Error code is returned negative, flip to positive to match errno
            Err(Error::System(-ret))
        } else {
            Ok(())
        }
    }

    /// [Unpin](https://facebookmicrosites.github.io/bpf/blog/2018/08/31/object-lifetime.html#bpffs)
    /// from bpffs
    pub fn unpin<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path_c = util::path_to_cstring(path)?;
        let path_ptr = path_c.as_ptr();

        let ret = unsafe { libbpf_sys::bpf_map__unpin(self.ptr, path_ptr) };
        if ret != 0 {
            // Error code is returned negative, flip to positive to match errno
            Err(Error::System(-ret))
        } else {
            Ok(())
        }
    }

    /// Returns map value as `Vec` of `u8`.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements.
    pub fn lookup(&self, key: &[u8], flags: MapFlags) -> Result<Option<Vec<u8>>> {
        if key.len() != self.key_size() as usize {
            return Err(Error::InvalidInput(format!(
                "key_size {} != {}",
                key.len(),
                self.key_size()
            )));
        };

        let mut out: Vec<u8> = Vec::with_capacity(self.value_size() as usize);

        let ret = unsafe {
            libbpf_sys::bpf_map_lookup_elem_flags(
                self.fd as i32,
                key.as_ptr() as *const c_void,
                out.as_mut_ptr() as *mut c_void,
                flags.bits,
            )
        };

        if ret == 0 {
            unsafe {
                out.set_len(self.value_size() as usize);
            }
            Ok(Some(out))
        } else {
            let errno = errno::errno();
            if errno::Errno::from_i32(errno) == errno::Errno::ENOENT {
                Ok(None)
            } else {
                Err(Error::System(errno))
            }
        }
    }

    /// Deletes an element from the map.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        if key.len() != self.key_size() as usize {
            return Err(Error::InvalidInput(format!(
                "key_size {} != {}",
                key.len(),
                self.key_size()
            )));
        };

        let ret = unsafe {
            libbpf_sys::bpf_map_delete_elem(self.fd as i32, key.as_ptr() as *const c_void)
        };

        if ret == 0 {
            Ok(())
        } else {
            Err(Error::System(errno::errno()))
        }
    }

    /// Same as [`Map::lookup()`] except this also deletes the key from the map.
    ///
    /// Note that this operation is currently only implemented in the kernel for [`MapType::Queue`]
    /// and [`MapType::Stack`].
    ///
    /// `key` must have exactly [`Map::key_size()`] elements.
    pub fn lookup_and_delete(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if key.len() != self.key_size() as usize {
            return Err(Error::InvalidInput(format!(
                "key_size {} != {}",
                key.len(),
                self.key_size()
            )));
        };

        let mut out: Vec<u8> = Vec::with_capacity(self.value_size() as usize);

        let ret = unsafe {
            libbpf_sys::bpf_map_lookup_and_delete_elem(
                self.fd as i32,
                key.as_ptr() as *const c_void,
                out.as_mut_ptr() as *mut c_void,
            )
        };

        if ret == 0 {
            unsafe {
                out.set_len(self.value_size() as usize);
            }
            Ok(Some(out))
        } else {
            let errno = errno::errno();
            if errno::Errno::from_i32(errno) == errno::Errno::ENOENT {
                Ok(None)
            } else {
                Err(Error::System(errno))
            }
        }
    }

    /// Update an element.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements. `value` must have exatly
    /// [`Map::value_size()`] elements.
    pub fn update(&mut self, key: &[u8], value: &[u8], flags: MapFlags) -> Result<()> {
        if key.len() != self.key_size() as usize {
            return Err(Error::InvalidInput(format!(
                "key_size {} != {}",
                key.len(),
                self.key_size()
            )));
        };

        if value.len() != self.value_size() as usize {
            return Err(Error::InvalidInput(format!(
                "value_size {} != {}",
                value.len(),
                self.value_size()
            )));
        };

        let ret = unsafe {
            libbpf_sys::bpf_map_update_elem(
                self.fd as i32,
                key.as_ptr() as *const c_void,
                value.as_ptr() as *const c_void,
                flags.bits,
            )
        };

        if ret == 0 {
            Ok(())
        } else {
            Err(Error::System(errno::errno()))
        }
    }
}

#[rustfmt::skip]
bitflags! {
    /// Flags to configure [`Map`] operations.
    pub struct MapFlags: u64 {
	const ANY      = 0;
	const NO_EXIST = 1;
	const EXIST    = 1 << 1;
	const LOCK     = 1 << 2;
    }
}

/// Type of a [`Map`]. Maps to `enum bpf_map_type` in kernel uapi.
#[non_exhaustive]
#[repr(u32)]
#[derive(Clone, TryFromPrimitive, PartialEq, Display)]
pub enum MapType {
    Unspec = 0,
    Hash,
    Array,
    ProgArray,
    PerfEventArray,
    PercpuHash,
    PercpuArray,
    StackTrace,
    CgroupArray,
    LruHash,
    LruPercpuHash,
    LpmTrie,
    ArrayOfMaps,
    HashOfMaps,
    Devmap,
    Sockmap,
    Cpumap,
    Xskmap,
    Sockhash,
    CgroupStorage,
    ReuseportSockarray,
    PercpuCgroupStorage,
    Queue,
    Stack,
    SkStorage,
    DevmapHash,
    StructOps,
    /// We choose to specify our own "unknown" type here b/c it's really up to the kernel
    /// to decide if it wants to reject the map. If it accepts it, it just means whoever
    /// using this library is a bit out of date.
    Unknown = u32::MAX,
}

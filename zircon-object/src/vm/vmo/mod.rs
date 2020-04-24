use {super::*, crate::object::*, alloc::sync::Arc, bitflags::bitflags, kernel_hal::PageTable};

mod paged;
mod physical;

use self::{paged::*, physical::*};
use core::ops::Deref;

/// Virtual Memory Objects
#[allow(clippy::len_without_is_empty)]
pub trait VMObjectTrait: Sync + Send {
    /// Read memory to `buf` from VMO at `offset`.
    fn read(&self, offset: usize, buf: &mut [u8]) -> ZxResult;

    /// Write memory from `buf` to VMO at `offset`.
    fn write(&self, offset: usize, buf: &[u8]) -> ZxResult;

    /// Get the length of VMO.
    fn len(&self) -> usize;

    /// Set the length of VMO.
    fn set_len(&self, len: usize);

    /// Unmap physical memory from `page_table`.
    fn unmap_from(&self, page_table: &mut PageTable, vaddr: VirtAddr, _offset: usize, len: usize) {
        // TODO _offset unused?
        let pages = len / PAGE_SIZE;
        page_table
            .unmap_cont(vaddr, pages)
            .expect("failed to unmap")
    }

    /// Commit a page.
    fn commit_page(&self, page_idx: usize, flags: MMUFlags) -> ZxResult<PhysAddr>;

    /// Commit allocating physical memory.
    fn commit(&self, offset: usize, len: usize) -> ZxResult;

    /// Decommit allocated physical memory.
    fn decommit(&self, offset: usize, len: usize) -> ZxResult;

    /// Create a child VMO.
    fn create_child(&self, offset: usize, len: usize) -> Arc<dyn VMObjectTrait>;

    fn append_mapping(&self, mapping: Arc<VmMapping>);

    fn complete_info(&self, info: &mut ZxInfoVmo);
}

pub struct VmObject {
    base: KObjectBase,
    parent_koid: KoID,
    _counter: CountHelper,
    resizable: bool,
    inner: Arc<dyn VMObjectTrait>,
}

impl_kobject!(VmObject);
define_count_helper!(VmObject);

impl VmObject {
    /// Create a new VMO backing on physical memory allocated in pages.
    pub fn new_paged(pages: usize) -> Arc<Self> {
        Arc::new(VmObject {
            base: KObjectBase::default(),
            parent_koid: 0,
            resizable: true,
            _counter: CountHelper::new(),
            inner: VMObjectPaged::new(pages),
        })
    }

    pub fn new_paged_with_resizable(resizable: bool, pages: usize) -> Arc<Self> {
        Arc::new(VmObject {
            base: KObjectBase::default(),
            parent_koid: 0,
            resizable,
            _counter: CountHelper::new(),
            inner: VMObjectPaged::new(pages),
        })
    }

    /// Create a new VMO representing a piece of contiguous physical memory.
    ///
    /// # Safety
    ///
    /// You must ensure nobody has the ownership of this piece of memory yet.
    #[allow(unsafe_code)]
    pub unsafe fn new_physical(paddr: PhysAddr, pages: usize) -> Arc<Self> {
        Arc::new(VmObject {
            base: KObjectBase::default(),
            parent_koid: 0,
            resizable: true,
            _counter: CountHelper::new(),
            inner: VMObjectPhysical::new(paddr, pages),
        })
    }

    pub fn create_child(&self, resizable: bool, offset: usize, len: usize) -> Arc<Self> {
        // error!("create_child: offset={:#x}, len={:#x}", offset, len);
        Arc::new(VmObject {
            base: KObjectBase::with_name(&self.base.name()),
            parent_koid: self.base.id,
            resizable,
            _counter: CountHelper::new(),
            inner: self.inner.create_child(offset, len),
        })
    }

    pub fn set_len(&self, len: usize) -> ZxResult {
        if self.resizable {
            self.inner.set_len(len);
            Ok(())
        } else {
            Err(ZxError::UNAVAILABLE)
        }
    }

    pub fn get_info(&self) -> ZxInfoVmo {
        let mut ret = ZxInfoVmo {
            koid: self.base.id,
            name: {
                let mut arr = [0u8; 32];
                let name = self.base.name();
                let length = name.len().min(32);
                arr[..length].copy_from_slice(&name.as_bytes()[..length]);
                arr
            },
            size: self.inner.len() as u64,
            parent_koid: self.parent_koid,
            flags: if self.resizable {
                VmoInfoFlags::RESIZABLE
            } else {
                VmoInfoFlags::empty()
            },
            ..Default::default()
        };
        self.inner.complete_info(&mut ret);
        ret
    }
}

impl Deref for VmObject {
    type Target = Arc<dyn VMObjectTrait>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Describes a VMO.
#[repr(C)]
#[derive(Default)]
pub struct ZxInfoVmo {
    /// The koid of this VMO.
    koid: KoID,
    /// The name of this VMO.
    name: [u8; 32],
    /// The size of this VMO; i.e., the amount of virtual address space it
    /// would consume if mapped.
    size: u64,
    /// If this VMO is a clone, the koid of its parent. Otherwise, zero.
    parent_koid: KoID,
    /// The number of clones of this VMO, if any.
    num_children: u64,
    /// The number of times this VMO is currently mapped into VMARs.
    num_mappings: u64,
    /// The number of unique address space we're mapped into.
    share_count: u64,
    /// Flags.
    pub flags: VmoInfoFlags,
    /// Padding.
    padding1: [u8; 4],
    /// If the type is `PAGED`, the amount of
    /// memory currently allocated to this VMO; i.e., the amount of physical
    /// memory it consumes. Undefined otherwise.
    committed_bytes: u64,
    /// If `flags & ZX_INFO_VMO_VIA_HANDLE`, the handle rights.
    /// Undefined otherwise.
    pub rights: Rights,
    /// VMO mapping cache policy.
    cache_policy: u32,
}

bitflags! {
    #[derive(Default)]
    pub struct VmoInfoFlags: u32 {
        const TYPE_PHYSICAL = 0;
        #[allow(clippy::identity_op)]
        const TYPE_PAGED    = 1 << 0;
        const RESIZABLE     = 1 << 1;
        const IS_COW_CLONE  = 1 << 2;
        const VIA_HANDLE    = 1 << 3;
        const VIA_MAPPING   = 1 << 4;
        const PAGER_BACKED  = 1 << 5;
        const CONTIGUOUS    = 1 << 6;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn read_write(vmo: &VmObject) {
        let mut buf = [0u8; 4];
        vmo.write(0, &[0, 1, 2, 3]).unwrap();
        vmo.read(0, &mut buf).unwrap();
        assert_eq!(&buf, &[0, 1, 2, 3]);
    }
}

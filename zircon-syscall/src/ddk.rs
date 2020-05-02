#![allow(warnings)]

use {
    super::*,
    zircon_object::{
        dev::*,
        resource::*,
    },
    bitflags::bitflags,
    kernel_hal::{PhysAddr, VirtAddr, DevVAddr},
    zircon_object::vm::{page_aligned, VmObject},
};

impl Syscall<'_> {
    pub fn sys_iommu_create(
        &self,
        resource: HandleValue,
        type_: u32,
        desc: UserInPtr<u8>,
        desc_size: usize,
        mut out: UserOutPtr<HandleValue>,
    ) -> ZxResult {
        info!(
            "iommu.create: resource={:#x}, type={:#x}, desc={:#x?} desc_size={:#x} out={:#x?}",
            resource, type_, desc, desc_size, out
        );
        let proc = self.thread.proc();
        proc.validate_resource(resource, ResourceKind::ROOT)?;
        if desc_size > IOMMU_MAX_DESC_LEN {
            return Err(ZxError::INVALID_ARGS);
        }
        if desc_size != IOMMU_DESC_SIZE {
            return Err(ZxError::INVALID_ARGS);
        }
        let copied_desc = desc.read_array(desc_size)?;
        let iommu = Iommu::create(type_, copied_desc, desc_size);
        let handle = proc.add_handle(Handle::new(iommu, Rights::DEFAULT_CHANNEL));
        info!("iommu handle value {:#x}", handle);
        out.write(handle)?;
        Ok(())
    }

    pub fn sys_bti_create(
        &self,
        iommu: HandleValue,
        options: u32,
        bti_id: u64,
        mut out: UserOutPtr<HandleValue>,
    ) -> ZxResult {
        info!(
            "bti.create: iommu={:#x}, options={:?}, bti_id={:#x?}",
            iommu, options, bti_id
        );
        let proc = self.thread.proc();
        if options != 0 {
            return Err(ZxError::INVALID_ARGS);
        }
        let iommu = proc.get_object::<Iommu>(iommu)?;
        if !iommu.is_valid_bus_txn_id() {
            return Err(ZxError::INVALID_ARGS);
        }
        let bti = Bti::create(iommu, bti_id);
        let handle = proc.add_handle(Handle::new(bti, Rights::DEFAULT_BTI));
        out.write(handle)?;
        Ok(())
    }

    pub fn sys_bti_pin(
        &self,
        bti: HandleValue,
        options: u32,
        vmo: HandleValue,
        offset: usize,
        size: usize,
        mut addrs: UserOutPtr<DevVAddr>,
        addrs_count: usize,
        mut out: UserOutPtr<HandleValue>,
    ) -> ZxResult {
        info!(
            "bti.pin: bti={:#x}, options={:?}, vmo={:#x}, offset={:#x}, size={:#x}, addrs: {:#x?}, addrs_count: {:#x}",
            bti, options, vmo, offset, size, addrs, addrs_count
        );
        let proc = self.thread.proc();
        let bti = proc.get_object_with_rights::<Bti>(bti, Rights::MAP)?;
        if !page_aligned(offset) || !page_aligned(size) {
            return Err(ZxError::INVALID_ARGS);
        }
        let (vmo, rights) = proc.get_object_and_rights::<VmObject>(vmo)?;
        if !rights.contains(Rights::MAP) {
            return Err(ZxError::ACCESS_DENIED);
        }

        let mut iommu_perms = IommuPerms::empty();
        let options = BtiOptions::from_bits_truncate(options);
        
        if options.contains(BtiOptions::PERM_READ) {
            if !rights.contains(Rights::READ) {
                return Err(ZxError::ACCESS_DENIED);
            }
            iommu_perms.insert(IommuPerms::PERM_READ)
        }

        if options.contains(BtiOptions::PERM_WRITE) {
            if !rights.contains(Rights::WRITE) {
                return Err(ZxError::ACCESS_DENIED);
            }
            iommu_perms.insert(IommuPerms::PERM_WRITE);
        }

        if options.contains(BtiOptions::PERM_EXECUTE) {
            // NOTE: Check Rights::READ instead of Rights::EXECUTE, 
            // because Rights::EXECUTE applies to the execution permission of the host CPU, 
            // but ZX_BTI_PERM_EXECUTE applies to transactions initiated by the bus device.
            if !rights.contains(Rights::READ) {
                return Err(ZxError::ACCESS_DENIED);
            }
            iommu_perms.insert(IommuPerms::PERM_EXECUTE);
        }
        
        if options.contains(BtiOptions::CONTIGUOUS) && options.contains(BtiOptions::COMPRESS) {
            return Err(ZxError::INVALID_ARGS);
        }
        if options.contains(BtiOptions::CONTIGUOUS) && !vmo.is_contiguous() {
            return Err(ZxError::INVALID_ARGS)
        }
        let compress_results = options.contains(BtiOptions::COMPRESS);
        let contiguous = options.contains(BtiOptions::CONTIGUOUS);
        
        let pmt = bti.pin(vmo, offset, size, iommu_perms)?;
        addrs.write_array(&pmt.as_ref().encode_addrs(compress_results, contiguous, addrs_count)?)?;

        let handle = proc.add_handle(Handle::new(pmt, Rights::INSPECT));
        out.write(handle)?;
        Ok(())
    }
}

const IOMMU_MAX_DESC_LEN: usize = 4096;
const IOMMU_DESC_SIZE: usize = 1;

bitflags! {
    struct BtiOptions: u32 {
        const PERM_READ             = 1 << 0;
        const PERM_WRITE            = 1 << 1;
        const PERM_EXECUTE          = 1 << 2;
        const COMPRESS              = 1 << 3;
        const CONTIGUOUS            = 1 << 4;
    }
}


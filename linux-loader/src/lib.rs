#![no_std]
#![feature(asm)]
#![feature(global_asm)]
#![deny(warnings, unused_must_use)]

extern crate alloc;
extern crate log;

use {
    alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec},
    kernel_hal_unix::{switch_to_kernel, switch_to_user},
    linux_syscall::{ProcessExt, Syscall},
    xmas_elf::{
        program::{Flags, ProgramHeader, SegmentData, Type},
        sections::SectionData,
        symbol_table::{DynEntry64, Entry},
        ElfFile,
    },
    zircon_object::{task::*, vm::*, ZxError, ZxResult},
};

mod abi;

pub fn run(libc_data: &[u8], args: Vec<String>, envs: Vec<String>) -> Arc<Process> {
    let job = Job::root();
    let proc = Process::create_linux(&job, "proc").unwrap();
    let thread = Thread::create(&proc, "thread", 0).unwrap();
    let vmar = proc.vmar();

    // libc.so
    let (base, elf) = {
        let elf = ElfFile::new(libc_data).unwrap();
        let size = elf.load_segment_size();
        let syscall_entry_offset = elf
            .get_symbol_address("rcore_syscall_entry")
            .expect("failed to locate syscall entry") as usize;
        let vmar = vmar.create_child(None, size).unwrap();
        let vmo = vmar.load_from_elf(&elf).unwrap();
        // fill syscall entry
        vmo.write(
            syscall_entry_offset,
            &(syscall_entry as usize).to_ne_bytes(),
        );
        elf.relocate(vmar.addr()).unwrap();
        (vmar.addr(), elf)
    };
    let entry = base + elf.header.pt2.entry_point() as usize;

    const STACK_SIZE: usize = 0x8000;
    let stack = Vec::<u8>::with_capacity(STACK_SIZE);
    let mut sp = (stack.as_ptr() as usize + STACK_SIZE) & !0xf;

    let info = abi::ProcInitInfo {
        args,
        envs,
        auxv: {
            let mut map = BTreeMap::new();
            map.insert(abi::AT_BASE, base);
            map.insert(abi::AT_PHDR, base + elf.header.pt2.ph_offset() as usize);
            map.insert(abi::AT_PHENT, elf.header.pt2.ph_entry_size() as usize);
            map.insert(abi::AT_PHNUM, elf.header.pt2.ph_count() as usize);
            map.insert(abi::AT_PAGESZ, PAGE_SIZE);
            map
        },
    };
    sp = unsafe { info.push_at(sp) };

    thread
        .start(entry, sp, 0, 0)
        .expect("failed to start main thread");
    proc
}

extern "C" fn syscall_entry(
    num: u32,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> isize {
    unsafe {
        switch_to_kernel();
    }
    let syscall = Syscall {
        thread: Thread::current(),
    };
    let ret = syscall.syscall(num, [a0, a1, a2, a3, a4, a5]);
    unsafe {
        switch_to_user();
    }
    ret
}

pub trait ElfExt {
    fn load_segment_size(&self) -> usize;
    fn get_symbol_address(&self, symbol: &str) -> Option<u64>;
    fn dynsym(&self) -> Result<&[DynEntry64], &'static str>;
    fn relocate(&self, base: usize) -> Result<(), &'static str>;
}

impl ElfExt for ElfFile<'_> {
    /// Get total size of all LOAD segments.
    fn load_segment_size(&self) -> usize {
        self.program_iter()
            .filter(|ph| ph.get_type().unwrap() == Type::Load)
            .map(|ph| pages((ph.virtual_addr() + ph.mem_size()) as usize))
            .max()
            .unwrap()
            * PAGE_SIZE
    }

    /// Get address of the given `symbol`.
    fn get_symbol_address(&self, symbol: &str) -> Option<u64> {
        for section in self.section_iter() {
            if let SectionData::SymbolTable64(entries) = section.get_data(self).unwrap() {
                for e in entries {
                    if e.get_name(self).unwrap() == symbol {
                        return Some(e.value());
                    }
                }
            }
        }
        None
    }

    fn dynsym(&self) -> Result<&[DynEntry64], &'static str> {
        match self
            .find_section_by_name(".dynsym")
            .ok_or(".dynsym not found")?
            .get_data(self)
            .map_err(|_| "corrupted .dynsym")?
        {
            SectionData::DynSymbolTable64(dsym) => Ok(dsym),
            _ => Err("bad .dynsym"),
        }
    }

    fn relocate(&self, base: usize) -> Result<(), &'static str> {
        let data = self
            .find_section_by_name(".rela.dyn")
            .ok_or(".rela.dyn not found")?
            .get_data(self)
            .map_err(|_| "corrupted .rela.dyn")?;
        let entries = match data {
            SectionData::Rela64(entries) => entries,
            _ => return Err("bad .rela.dyn"),
        };
        let dynsym = self.dynsym()?;
        for entry in entries {
            const REL_GOT: u32 = 6;
            const REL_PLT: u32 = 7;
            const REL_RELATIVE: u32 = 8;
            match entry.get_type() {
                REL_GOT | REL_PLT => {
                    let dynsym = &dynsym[entry.get_symbol_table_index() as usize];
                    let symval = if dynsym.shndx() == 0 {
                        let name = dynsym.get_name(self)?;
                        panic!("need to find symbol: {:?}", name);
                    } else {
                        base + dynsym.value() as usize
                    };
                    let value = symval + entry.get_addend() as usize;
                    unsafe {
                        let ptr = (base + entry.get_offset() as usize) as *mut usize;
                        ptr.write(value);
                    }
                }
                REL_RELATIVE => {
                    let value = base + entry.get_addend() as usize;
                    unsafe {
                        let ptr = (base + entry.get_offset() as usize) as *mut usize;
                        ptr.write(value);
                    }
                }
                t => unimplemented!("unknown type: {}", t),
            }
        }
        Ok(())
    }
}

pub trait VmarExt {
    fn load_from_elf(&self, elf: &ElfFile) -> ZxResult<Arc<VMObjectPaged>>;
}

impl VmarExt for VmAddressRegion {
    /// Create `VMObject` from all LOAD segments of `elf` and map them to this VMAR.
    /// Return the first `VMObject`.
    fn load_from_elf(&self, elf: &ElfFile) -> ZxResult<Arc<VMObjectPaged>> {
        let mut first_vmo = None;
        for ph in elf.program_iter() {
            if ph.get_type().unwrap() != Type::Load {
                continue;
            }
            let vmo = make_vmo(&elf, ph)?;
            let offset = ph.virtual_addr() as usize / PAGE_SIZE * PAGE_SIZE;
            let flags = ph.flags().to_mmu_flags();
            self.map_at(offset, vmo.clone(), 0, vmo.len(), flags)?;
            first_vmo.get_or_insert(vmo);
        }
        Ok(first_vmo.unwrap())
    }
}

trait FlagsExt {
    fn to_mmu_flags(&self) -> MMUFlags;
}

impl FlagsExt for Flags {
    fn to_mmu_flags(&self) -> MMUFlags {
        let mut flags = MMUFlags::empty();
        if self.is_read() {
            flags.insert(MMUFlags::READ);
        }
        if self.is_write() {
            flags.insert(MMUFlags::WRITE);
        }
        if self.is_execute() {
            flags.insert(MMUFlags::EXECUTE);
        }
        flags
    }
}

fn make_vmo(elf: &ElfFile, ph: ProgramHeader) -> ZxResult<Arc<VMObjectPaged>> {
    assert_eq!(ph.get_type().unwrap(), Type::Load);
    let page_offset = ph.virtual_addr() as usize % PAGE_SIZE;
    let pages = pages(ph.mem_size() as usize + page_offset);
    let vmo = VMObjectPaged::new(pages);
    let data = match ph.get_data(&elf).unwrap() {
        SegmentData::Undefined(data) => data,
        _ => return Err(ZxError::INVALID_ARGS),
    };
    vmo.write(page_offset, data);
    Ok(vmo)
}

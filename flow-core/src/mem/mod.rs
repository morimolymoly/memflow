use crate::address::{Address, Length};
use crate::arch::{Architecture, InstructionSet};
use crate::Result;

use dataview::Pod;
use std::ffi::CString;

// generic traits
pub trait PhysicalMemoryTrait {
    fn phys_read(&mut self, addr: Address, out: &mut [u8]) -> Result<()>;
    fn phys_write(&mut self, addr: Address, data: &[u8]) -> Result<()>;

    // TODO:
    // - check endianess here and return an error
    // - better would be to convert endianess with word alignment from addr
    fn phys_read_pod<T: Pod>(&mut self, addr: Address, out: &mut T) -> Result<()> {
        self.phys_read(addr, out.as_bytes_mut())
    }

    fn phys_write_pod<T: Pod>(&mut self, addr: Address, data: &T) -> Result<()> {
        self.phys_write(addr, data.as_bytes())
    }
}

pub trait VirtualMemoryTrait {
    fn virt_read(
        &mut self,
        arch: Architecture,
        dtb: Address,
        addr: Address,
        out: &mut [u8],
    ) -> Result<()>;

    fn virt_write(
        &mut self,
        arch: Architecture,
        dtb: Address,
        addr: Address,
        data: &[u8],
    ) -> Result<()>;

    // TODO:
    // - check endianess here and return an error
    // - better would be to convert endianess with word alignment from addr
    fn virt_read_pod<T: Pod>(
        &mut self,
        arch: Architecture,
        dtb: Address,
        addr: Address,
        out: &mut T,
    ) -> Result<()> {
        self.virt_read(arch, dtb, addr, out.as_bytes_mut())
    }

    fn virt_write_pod<T: Pod>(
        &mut self,
        arch: Architecture,
        dtb: Address,
        addr: Address,
        data: &T,
    ) -> Result<()> {
        self.virt_write(arch, dtb, addr, data.as_bytes())
    }
}

pub struct VirtualMemory<'a, T: VirtualMemoryTrait> {
    mem: &'a mut T,
    sys_arch: Architecture,
    proc_arch: Architecture,
    dtb: Address,
}

impl<'a, T: VirtualMemoryTrait> VirtualMemory<'a, T> {
    pub fn with(mem: &'a mut T, sys_arch: Architecture, dtb: Address) -> Self {
        Self {
            mem,
            sys_arch,
            proc_arch: sys_arch,
            dtb,
        }
    }

    pub fn with_proc_arch(
        mem: &'a mut T,
        sys_arch: Architecture,
        proc_arch: Architecture,
        dtb: Address,
    ) -> Self {
        Self {
            mem,
            sys_arch,
            proc_arch,
            dtb,
        }
    }

    pub fn sys_arch(&self) -> Architecture {
        self.sys_arch
    }

    pub fn proc_arch(&self) -> Architecture {
        self.proc_arch
    }

    pub fn dtb(&self) -> Address {
        self.dtb
    }

    // self.mem wrappers
    pub fn virt_read(&mut self, addr: Address, out: &mut [u8]) -> Result<()> {
        self.mem.virt_read(self.sys_arch, self.dtb, addr, out)
    }

    pub fn virt_write(&mut self, addr: Address, data: &[u8]) -> Result<()> {
        self.mem.virt_write(self.sys_arch, self.dtb, addr, data)
    }

    pub fn virt_read_pod<U: Pod>(&mut self, addr: Address, out: &mut U) -> Result<()> {
        self.mem.virt_read_pod(self.sys_arch, self.dtb, addr, out)
    }

    pub fn virt_write_pod<U: Pod>(&mut self, addr: Address, data: &U) -> Result<()> {
        self.mem.virt_write_pod(self.sys_arch, self.dtb, addr, data)
    }

    // custom read wrappers
    pub fn virt_read_addr32(&mut self, addr: Address) -> Result<Address> {
        let mut res = 0u32;
        self.virt_read_pod(addr, &mut res)?;
        Ok(Address::from(res))
    }

    pub fn virt_read_addr64(&mut self, addr: Address) -> Result<Address> {
        let mut res = 0u64;
        self.virt_read_pod(addr, &mut res)?;
        Ok(Address::from(res))
    }

    pub fn virt_read_addr(&mut self, addr: Address) -> Result<Address> {
        match self.proc_arch.instruction_set {
            InstructionSet::X86 => self.virt_read_addr32(addr),
            InstructionSet::X86Pae => self.virt_read_addr32(addr),
            InstructionSet::X64 => self.virt_read_addr64(addr),
        }
    }

    // TODO: read into slice?
    pub fn virt_read_cstr(&mut self, addr: Address, len: Length) -> Result<String> {
        let mut buf = vec![0; len.as_usize()];
        self.virt_read(addr, &mut buf)?;
        if let Some((n, _)) = buf.iter().enumerate().filter(|(_, c)| **c == 0_u8).nth(0) {
            buf.truncate(n);
        }
        let v = CString::new(buf)?;
        Ok(String::from(v.to_string_lossy()))
    }

    // TODO: read into slice?
    pub fn virt_read_cstr_ptr(&mut self, addr: Address) -> Result<String> {
        let ptr = self.virt_read_addr(addr)?;
        self.virt_read_cstr(ptr, Length::from_kb(2))
    }

    pub fn virt_read_addr_chain(
        &mut self,
        base_addr: Address,
        offsets: Vec<Length>,
    ) -> Result<Address> {
        offsets
            .iter()
            .try_fold(base_addr, |c, &a| self.virt_read_addr(c + a))
    }
}

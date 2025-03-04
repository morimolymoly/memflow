use crate::architecture::ArchitectureObj;
use crate::error::Result;
use crate::iter::PageChunks;
use crate::mem::mem_data::*;
use crate::mem::phys_mem::*;
use crate::types::{cache::CacheValidator, umem, Address, PageType, PhysicalAddress};

use std::alloc::{alloc, alloc_zeroed, dealloc, Layout};

use bumpalo::{collections::Vec as BumpVec, Bump};

pub enum PageValidity<'a> {
    Invalid,
    Validatable(&'a mut [u8]),
    ToBeValidated,
    Valid(&'a mut [u8]),
}

pub struct CacheEntry<'a> {
    pub address: Address,
    pub validity: PageValidity<'a>,
}

impl<'a> CacheEntry<'a> {
    pub fn with(address: Address, validity: PageValidity<'a>) -> Self {
        Self { address, validity }
    }
}

pub struct PageCache<'a, T> {
    address: Box<[Address]>,
    page_refs: Box<[Option<&'a mut [u8]>]>,
    address_once_validated: Box<[Address]>,
    page_size: usize,
    page_type_mask: PageType,
    pub validator: T,
    cache_ptr: *mut u8,
    cache_layout: Layout,
}

unsafe impl<'a, T> Send for PageCache<'a, T> {}

#[allow(clippy::needless_option_as_deref)]
impl<'a, T: CacheValidator> PageCache<'a, T> {
    pub fn new(arch: ArchitectureObj, size: usize, page_type_mask: PageType, validator: T) -> Self {
        Self::with_page_size(arch.page_size(), size, page_type_mask, validator)
    }

    pub fn with_page_size(
        page_size: usize,
        size: usize,
        page_type_mask: PageType,
        mut validator: T,
    ) -> Self {
        let cache_entries = size / page_size;

        let layout = Layout::from_size_align(cache_entries * page_size, page_size).unwrap();

        let cache_ptr = unsafe { alloc_zeroed(layout) };

        let page_refs = (0..cache_entries)
            .map(|i| unsafe {
                std::mem::transmute(std::slice::from_raw_parts_mut(
                    cache_ptr.add(i * page_size),
                    page_size,
                ))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        validator.allocate_slots(cache_entries);

        Self {
            address: vec![Address::INVALID; cache_entries].into_boxed_slice(),
            page_refs,
            address_once_validated: vec![Address::INVALID; cache_entries].into_boxed_slice(),
            page_size,
            page_type_mask,
            validator,
            cache_ptr,
            cache_layout: layout,
        }
    }

    fn page_index(&self, addr: Address) -> usize {
        ((addr.as_page_aligned(self.page_size).to_umem() / self.page_size as umem)
            % (self.address.len() as umem)) as usize
    }

    fn take_page(&mut self, addr: Address, skip_validator: bool) -> PageValidity<'a> {
        let page_index = self.page_index(addr);

        let bufopt = std::mem::replace(&mut self.page_refs[page_index], None);

        if let Some(buf) = bufopt {
            if self.address[page_index] == addr.as_page_aligned(self.page_size)
                && (skip_validator || self.validator.is_slot_valid(page_index))
            {
                PageValidity::Valid(buf)
            } else if self.address_once_validated[page_index]
                == addr.as_page_aligned(self.page_size)
                || self.address_once_validated[page_index] == Address::INVALID
            {
                PageValidity::Validatable(buf)
            } else {
                PageValidity::Invalid
            }
        } else if self.address_once_validated[page_index] == addr.as_page_aligned(self.page_size) {
            PageValidity::ToBeValidated
        } else {
            PageValidity::Invalid
        }
    }

    fn put_page(&mut self, addr: Address, page: &'a mut [u8]) {
        let page_index = self.page_index(addr);
        debug_assert!(self.page_refs[page_index].is_none());
        self.page_refs[page_index] = Some(page);
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn is_cached_page_type(&self, page_type: PageType) -> bool {
        self.page_type_mask.contains(page_type)
    }

    pub fn cached_page_mut(&mut self, addr: Address, skip_validator: bool) -> CacheEntry<'a> {
        let page_size = self.page_size;
        let aligned_addr = addr.as_page_aligned(page_size);
        CacheEntry {
            address: aligned_addr,
            validity: self.take_page(addr, skip_validator),
        }
    }

    pub fn put_entry(&mut self, entry: CacheEntry<'a>) {
        match entry.validity {
            PageValidity::Valid(buf) | PageValidity::Validatable(buf) => {
                self.put_page(entry.address, buf)
            }
            _ => {}
        }
    }

    pub fn mark_page_for_validation(&mut self, addr: Address) {
        let idx = self.page_index(addr);
        let aligned_addr = addr.as_page_aligned(self.page_size);
        self.address_once_validated[idx] = aligned_addr;
    }

    pub fn cancel_page_validation(&mut self, addr: Address, page_buf: &'a mut [u8]) {
        let idx = self.page_index(addr);
        // We could leave it in previous validity state,
        // but the buffer could have been partially written...
        if self.address_once_validated[idx] == addr {
            self.invalidate_page_raw(addr);
            self.put_page(addr, page_buf);
        }
    }

    pub fn validate_page(&mut self, addr: Address, page_buf: &'a mut [u8]) {
        let idx = self.page_index(addr);
        self.address[idx] = addr;
        self.address_once_validated[idx] = Address::INVALID;
        self.validator.validate_slot(idx);
        self.put_page(addr, page_buf);
    }

    pub fn invalidate_page_raw(&mut self, addr: Address) {
        let idx = self.page_index(addr);
        self.validator.invalidate_slot(idx);
        self.address[idx] = Address::INVALID;
        self.address_once_validated[idx] = Address::INVALID;
    }

    pub fn invalidate_page(&mut self, addr: Address, page_type: PageType) {
        if self.page_type_mask.contains(page_type) {
            self.invalidate_page_raw(addr)
        }
    }

    pub fn split_to_chunks(
        CTup3(addr, meta_addr, out): PhysicalReadData<'_>,
        page_size: usize,
    ) -> impl PhysicalReadIterator<'_> {
        (meta_addr, out).page_chunks(addr.address(), page_size).map(
            move |(paddr, (meta_addr, chunk))| {
                CTup3(
                    PhysicalAddress::with_page(paddr, addr.page_type(), addr.page_size() as umem),
                    meta_addr,
                    chunk,
                )
            },
        )
    }

    // TODO: do this properly
    pub fn cached_read<'b, F: PhysicalMemory>(
        &mut self,
        mem: &mut F,
        MemOps {
            inp: mut iter,
            out: mut cb_out,
            out_fail: mut cb_fail,
        }: PhysicalReadMemOps,
        arena: &'b Bump,
    ) -> Result<()> {
        let page_size = self.page_size;

        {
            let mut next = iter.next();
            let mut clist = BumpVec::new_in(arena);
            let mut wlist = BumpVec::new_in(arena);
            let mut wlistcache = BumpVec::new_in(arena);

            while let Some(CTup3(addr, meta_addr, out)) = next {
                if self.is_cached_page_type(addr.page_type()) {
                    (meta_addr, out)
                        .page_chunks(addr.address(), page_size)
                        .for_each(|(paddr, (meta_addr, chunk))| {
                            let mut prd = CTup3(
                                PhysicalAddress::with_page(
                                    paddr,
                                    addr.page_type(),
                                    addr.page_size() as umem,
                                ),
                                meta_addr,
                                chunk,
                            );

                            let cached_page = self.cached_page_mut(prd.0.address(), false);

                            match cached_page.validity {
                                PageValidity::Valid(buf) => {
                                    let aligned_addr = paddr.as_page_aligned(self.page_size);
                                    let start = paddr - aligned_addr;
                                    let cached_buf = buf
                                        .split_at_mut(start as usize)
                                        .1
                                        .split_at_mut(prd.2.len())
                                        .0;
                                    prd.2.copy_from_slice(cached_buf);
                                    opt_call(cb_out.as_deref_mut(), CTup2(prd.1, prd.2));
                                    self.put_page(cached_page.address, buf);
                                }
                                PageValidity::Validatable(buf) => {
                                    clist.push(prd);
                                    wlistcache.push(CTup3(
                                        PhysicalAddress::from(cached_page.address),
                                        meta_addr,
                                        buf.into(),
                                    ));
                                    self.mark_page_for_validation(cached_page.address);
                                }
                                PageValidity::ToBeValidated => {
                                    clist.push(prd);
                                }
                                PageValidity::Invalid => {
                                    wlist.push(prd);
                                }
                            }
                        });
                } else {
                    wlist.push(CTup3(addr, meta_addr, out));
                }

                next = iter.next();

                if next.is_none()
                    || wlist.len() >= 64
                    || wlistcache.len() >= 64
                    || clist.len() >= 64
                {
                    if !wlist.is_empty() {
                        {
                            let mut drain = wlist.drain(..);
                            mem.phys_read_raw_iter(MemOps {
                                inp: (&mut drain).into(),
                                out_fail: cb_fail.as_deref_mut(),
                                out: cb_out.as_deref_mut(),
                            })?;
                        }
                        wlist.clear();
                    }

                    if !wlistcache.is_empty() {
                        let mut iter =
                            wlistcache
                                .iter()
                                .map(|CTup3(addr, _, buf): &PhysicalReadData| {
                                    CTup3(*addr, addr.address(), buf.into())
                                });

                        let callback = &mut |CTup2(addr, buf): ReadData<'a>| {
                            self.validate_page(addr, buf.into());
                            true
                        };

                        let mut callback = callback.into();

                        mem.phys_read_raw_iter(MemOps {
                            inp: (&mut iter).into(),
                            out: Some(&mut callback),
                            out_fail: None,
                        })?;

                        wlistcache.into_iter().for_each(|CTup3(addr, _, buf)| {
                            self.cancel_page_validation(addr.address(), buf.into());
                        });

                        wlistcache = BumpVec::new_in(arena);
                    }

                    while let Some(CTup3(addr, meta_addr, mut out)) = clist.pop() {
                        let cached_page = self.cached_page_mut(addr.address(), false);
                        let aligned_addr = cached_page.address.as_page_aligned(self.page_size);

                        let start = addr.address() - aligned_addr;

                        if let PageValidity::Valid(buf) = cached_page.validity {
                            let cached_buf =
                                buf.split_at_mut(start as usize).1.split_at_mut(out.len()).0;
                            out.copy_from_slice(cached_buf);
                            self.put_page(cached_page.address, buf);
                            opt_call(cb_out.as_deref_mut(), CTup2(meta_addr, out));
                        } else {
                            opt_call(cb_fail.as_deref_mut(), CTup2(meta_addr, out));
                        }
                    }
                }
            }

            Ok(())
        }
    }
}

impl<'a, T> Clone for PageCache<'a, T>
where
    T: CacheValidator + Clone,
{
    fn clone(&self) -> Self {
        let page_size = self.page_size;
        let page_type_mask = self.page_type_mask;
        let validator = self.validator.clone();

        let cache_entries = self.address.len();

        let layout = Layout::from_size_align(cache_entries * page_size, page_size).unwrap();

        let cache_ptr = unsafe { alloc(layout) };

        unsafe {
            std::ptr::copy_nonoverlapping(self.cache_ptr, cache_ptr, cache_entries * page_size);
        };

        let page_refs = (0..cache_entries)
            .map(|i| unsafe {
                std::mem::transmute(std::slice::from_raw_parts_mut(
                    cache_ptr.add(i * page_size),
                    page_size,
                ))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self {
            address: vec![Address::INVALID; cache_entries].into_boxed_slice(),
            page_refs,
            address_once_validated: vec![Address::INVALID; cache_entries].into_boxed_slice(),
            page_size,
            page_type_mask,
            validator,
            cache_ptr,
            cache_layout: layout,
        }
    }
}

impl<'a, T> Drop for PageCache<'a, T> {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.cache_ptr, self.cache_layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::x86;
    use crate::cglue::ForwardMut;
    use crate::dummy::{DummyMemory, DummyOs};
    use crate::mem::{CachedPhysicalMemory, MemoryView, VirtualDma};
    use crate::types::{cache::TimedCacheValidator, size, Address, PhysicalAddress};

    use coarsetime::Duration;
    use rand::{thread_rng, Rng};

    fn diff_regions<'a>(
        mut r1: &'a [u8],
        mut r2: &'a [u8],
        diff_size: usize,
    ) -> Vec<(usize, &'a [u8], &'a [u8])> {
        let mut diffs = vec![];

        assert!(r1.len() == r2.len());

        let mut cidx = 0;

        while !r1.is_empty() {
            let splitc = core::cmp::min(r1.len(), diff_size);
            let (r1l, r1r) = r1.split_at(splitc);
            let (r2l, r2r) = r2.split_at(splitc);
            r1 = r1r;
            r2 = r2r;

            if r1l != r2l {
                diffs.push((cidx, r1l, r2l));
            }

            cidx += splitc;
        }

        diffs
    }

    #[test]
    fn cloned_validity() {
        let mem = DummyMemory::new(size::mb(32));
        let mut dummy_os = DummyOs::with_seed(mem, 0);

        let cmp_buf = [143u8; 16];
        let write_addr = PhysicalAddress::NULL;

        dummy_os.as_mut().phys_write(write_addr, &cmp_buf).unwrap();
        let arch = x86::x64::ARCH;

        let mut mem = CachedPhysicalMemory::builder(dummy_os.into_inner())
            .validator(TimedCacheValidator::new(Duration::from_secs(100)))
            .page_type_mask(PageType::UNKNOWN)
            .arch(arch)
            .build()
            .unwrap();

        let mut read_buf = [0u8; 16];
        mem.phys_read_into(write_addr, &mut read_buf).unwrap();
        assert_eq!(read_buf, cmp_buf);

        let mut cloned_mem = mem.clone();

        let mut cloned_read_buf = [0u8; 16];
        cloned_mem
            .phys_read_into(write_addr, &mut cloned_read_buf)
            .unwrap();
        assert_eq!(cloned_read_buf, cmp_buf);
    }

    /// Test cached memory read both with a random seed and a predetermined one.
    ///
    /// The predetermined seed was found to be problematic when it comes to memory overlap
    #[test]
    fn big_virt_buf() {
        for &seed in &[0x3ffd_235c_5194_dedf, thread_rng().gen_range(0..!0u64)] {
            let dummy_mem = DummyMemory::new(size::mb(512));
            let mut dummy_os = DummyOs::with_seed(dummy_mem, seed);

            let virt_size = size::mb(18);
            let mut test_buf = vec![0_u64; virt_size / 8];

            for i in &mut test_buf {
                *i = thread_rng().gen::<u64>();
            }

            let test_buf =
                unsafe { std::slice::from_raw_parts(test_buf.as_ptr() as *const u8, virt_size) };

            let (dtb, virt_base) = dummy_os.alloc_dtb(virt_size, test_buf);
            let arch = x86::x64::ARCH;
            println!("dtb={:x} virt_base={:x} seed={:x}", dtb, virt_base, seed);
            let translator = x86::x64::new_translator(dtb);

            let mut buf_nocache = vec![0_u8; test_buf.len()];
            {
                let mut virt_mem = VirtualDma::new(dummy_os.forward_mut(), arch, translator);
                virt_mem
                    .read_raw_into(virt_base, buf_nocache.as_mut_slice())
                    .unwrap();
            }

            assert!(
                buf_nocache == test_buf,
                "buf_nocache ({:?}..{:?}) != test_buf ({:?}..{:?})",
                &buf_nocache[..16],
                &buf_nocache[buf_nocache.len() - 16..],
                &test_buf[..16],
                &test_buf[test_buf.len() - 16..]
            );

            let cache = PageCache::new(
                arch,
                size::mb(2),
                PageType::PAGE_TABLE | PageType::READ_ONLY,
                TimedCacheValidator::new(Duration::from_secs(100)),
            );
            let mut mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);
            let mut buf_cache = vec![0_u8; buf_nocache.len()];
            {
                let mut virt_mem = VirtualDma::new(mem_cache.forward_mut(), arch, translator);
                virt_mem
                    .read_raw_into(virt_base, buf_cache.as_mut_slice())
                    .unwrap();
            }

            assert!(
                buf_nocache == buf_cache,
                "buf_nocache\n({:?}..{:?}) != buf_cache\n({:?}..{:?})\nDiff:\n{:?}",
                &buf_nocache[..16],
                &buf_nocache[buf_nocache.len() - 16..],
                &buf_cache[..16],
                &buf_cache[buf_cache.len() - 16..],
                diff_regions(&buf_nocache, &buf_cache, 32)
            );
        }
    }

    #[test]
    fn cache_invalidity_cached() {
        let dummy_mem = DummyMemory::new(size::mb(64));
        let mut dummy_os = DummyOs::new(dummy_mem);
        let mem_ptr = dummy_os.as_mut() as *mut DummyMemory;
        let virt_size = size::mb(8);
        let mut buf_start = vec![0_u8; 64];
        for (i, item) in buf_start.iter_mut().enumerate() {
            *item = (i % 256) as u8;
        }
        let (dtb, virt_base) = dummy_os.alloc_dtb(virt_size, &buf_start);
        let arch = x86::x64::ARCH;
        let translator = x86::x64::new_translator(dtb);

        let cache = PageCache::new(
            arch,
            size::mb(2),
            PageType::PAGE_TABLE | PageType::READ_ONLY | PageType::WRITEABLE,
            TimedCacheValidator::new(Duration::from_secs(100)),
        );

        let mut mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);

        //Modifying the memory from other channels should leave the cached page unchanged
        let mut cached_buf = vec![0_u8; 64];
        {
            let mut virt_mem = VirtualDma::new(mem_cache.forward_mut(), arch, translator);
            virt_mem
                .read_raw_into(virt_base, cached_buf.as_mut_slice())
                .unwrap();
        }

        let mut write_buf = cached_buf.clone();
        write_buf[16..20].copy_from_slice(&[255, 255, 255, 255]);
        {
            let mut virt_mem = VirtualDma::new(
                unsafe { mem_ptr.as_mut().unwrap() }.forward_mut(),
                arch,
                translator,
            );
            virt_mem.write_raw(virt_base, write_buf.as_slice()).unwrap();
        }

        let mut check_buf = vec![0_u8; 64];
        {
            let mut virt_mem = VirtualDma::new(mem_cache.forward_mut(), arch, translator);
            virt_mem
                .read_raw_into(virt_base, check_buf.as_mut_slice())
                .unwrap();
        }

        assert_eq!(cached_buf, check_buf);
        assert_ne!(check_buf, write_buf);
    }

    #[test]
    fn cache_invalidity_non_cached() {
        let dummy_mem = DummyMemory::new(size::mb(64));
        let mut dummy_os = DummyOs::new(dummy_mem);
        let mem_ptr = dummy_os.as_mut() as *mut DummyMemory;
        let virt_size = size::mb(8);
        let mut buf_start = vec![0_u8; 64];
        for (i, item) in buf_start.iter_mut().enumerate() {
            *item = (i % 256) as u8;
        }
        let (dtb, virt_base) = dummy_os.alloc_dtb(virt_size, &buf_start);
        let arch = x86::x64::ARCH;
        let translator = x86::x64::new_translator(dtb);

        //alloc_dtb creates a page table with all writeable pages, we disable cache for them
        let cache = PageCache::new(
            arch,
            size::mb(2),
            PageType::PAGE_TABLE | PageType::READ_ONLY,
            TimedCacheValidator::new(Duration::from_secs(100)),
        );

        let mut mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);

        //Modifying the memory from other channels should leave the cached page unchanged
        let mut cached_buf = vec![0_u8; 64];
        {
            let mut virt_mem = VirtualDma::new(mem_cache.forward_mut(), arch, translator);
            virt_mem
                .read_raw_into(virt_base, cached_buf.as_mut_slice())
                .unwrap();
        }

        let mut write_buf = cached_buf.clone();
        write_buf[16..20].copy_from_slice(&[255, 255, 255, 255]);
        {
            let mut virt_mem = VirtualDma::new(
                unsafe { mem_ptr.as_mut().unwrap() }.forward_mut(),
                arch,
                translator,
            );
            virt_mem.write_raw(virt_base, write_buf.as_slice()).unwrap();
        }

        let mut check_buf = vec![0_u8; 64];
        {
            let mut virt_mem = VirtualDma::new(mem_cache.forward_mut(), arch, translator);
            virt_mem
                .read_raw_into(virt_base, check_buf.as_mut_slice())
                .unwrap();
        }

        assert_ne!(cached_buf, check_buf);
        assert_eq!(check_buf, write_buf);
    }

    /// Test overlap of page cache.
    ///
    /// This test will fail if the page marks a memory region for copying from the cache, but also
    /// caches a different page in the entry before the said copy is operation is made.
    #[test]
    fn cache_phys_mem_overlap() {
        let dummy_mem = DummyMemory::new(size::mb(16));
        let mut dummy_os = DummyOs::new(dummy_mem);

        let buf_size = size::kb(8);
        let mut buf_start = vec![0_u8; buf_size];
        for (i, item) in buf_start.iter_mut().enumerate() {
            *item = ((i / 115) % 256) as u8;
        }

        let address = Address::NULL;

        let addr = PhysicalAddress::with_page(address, PageType::default().write(false), 0x1000);

        dummy_os
            .as_mut()
            .phys_write(addr, buf_start.as_slice())
            .unwrap();

        let arch = x86::x64::ARCH;

        let cache = PageCache::new(
            arch,
            size::kb(4),
            PageType::PAGE_TABLE | PageType::READ_ONLY,
            TimedCacheValidator::new(Duration::from_secs(100)),
        );

        let mut mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);

        let mut buf_1 = vec![0_u8; buf_size];
        mem_cache
            .phys_read_into(addr, buf_1.as_mut_slice())
            .unwrap();
        println!("READ CACHED {:p}", buf_1.as_ptr());
        println!("BS {:?} {:p}", &buf_start[..128], buf_start.as_ptr());
        println!("B1 {:?} {:p}", &buf_1[..128], buf_1.as_ptr());
        mem_cache
            .phys_read_into(addr, buf_1.as_mut_slice())
            .unwrap();

        println!("BS {:?} {:p}", &buf_start[..128], buf_start.as_ptr());
        println!("B1 {:?} {:p}", &buf_1[..128], buf_1.as_ptr());

        assert!(
            buf_start == buf_1,
            "buf_start != buf_1; diff: {:?}",
            diff_regions(&buf_start, &buf_1, 128)
        );

        let addr = PhysicalAddress::with_page(
            address + size::kb(4),
            PageType::default().write(false),
            0x1000,
        );

        let mut buf_2 = vec![0_u8; buf_size];
        mem_cache
            .phys_read_into(addr, buf_2.as_mut_slice())
            .unwrap();

        assert!(
            buf_1[0x1000..] == buf_2[..0x1000],
            "buf_1 != buf_2; diff: {:?}",
            diff_regions(&buf_1[0x1000..], &buf_2[..0x1000], 128)
        );
    }

    #[test]
    fn cache_phys_mem() {
        let dummy_mem = DummyMemory::new(size::mb(16));
        let mut dummy_os = DummyOs::new(dummy_mem);

        let mut buf_start = vec![0_u8; 64];
        for (i, item) in buf_start.iter_mut().enumerate() {
            *item = (i % 256) as u8;
        }

        let address = Address::from(0x5323);

        let addr = PhysicalAddress::with_page(address, PageType::default().write(false), 0x1000);

        dummy_os
            .as_mut()
            .phys_write(addr, buf_start.as_slice())
            .unwrap();

        let arch = x86::x64::ARCH;

        let cache = PageCache::new(
            arch,
            size::mb(2),
            PageType::PAGE_TABLE | PageType::READ_ONLY,
            TimedCacheValidator::new(Duration::from_secs(100)),
        );

        let mut mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);

        let mut buf_1 = vec![0_u8; 64];
        mem_cache
            .phys_read_into(addr, buf_1.as_mut_slice())
            .unwrap();

        assert_eq!(buf_start, buf_1);
    }
    #[test]
    fn cache_phys_mem_diffpages() {
        let dummy_mem = DummyMemory::new(size::mb(16));
        let mut dummy_os = DummyOs::new(dummy_mem);

        let mut buf_start = vec![0_u8; 64];
        for (i, item) in buf_start.iter_mut().enumerate() {
            *item = (i % 256) as u8;
        }

        let address = Address::from(0x5323);

        let addr1 = PhysicalAddress::with_page(address, PageType::default().write(false), 0x1000);

        let addr2 = PhysicalAddress::with_page(address, PageType::default().write(false), 0x100);

        dummy_os
            .as_mut()
            .phys_write(addr1, buf_start.as_slice())
            .unwrap();

        let cache = PageCache::with_page_size(
            0x10,
            0x10,
            PageType::PAGE_TABLE | PageType::READ_ONLY,
            TimedCacheValidator::new(Duration::from_secs(100)),
        );

        let mut mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);

        let mut buf_1 = vec![0_u8; 64];
        mem_cache
            .phys_read_into(addr1, buf_1.as_mut_slice())
            .unwrap();

        assert_eq!(buf_start, buf_1);

        let mut buf_2 = vec![0_u8; 64];
        mem_cache
            .phys_read_into(addr2, buf_2.as_mut_slice())
            .unwrap();

        assert_eq!(buf_1, buf_2);

        let mut buf_3 = vec![0_u8; 64];
        mem_cache
            .phys_read_into(addr2, buf_3.as_mut_slice())
            .unwrap();

        assert_eq!(buf_2, buf_3);
    }

    #[test]
    fn writeback() {
        let dummy_mem = DummyMemory::new(size::mb(16));
        let mut dummy_os = DummyOs::new(dummy_mem);
        let virt_size = size::mb(8);
        let mut buf_start = vec![0_u8; 64];
        for (i, item) in buf_start.iter_mut().enumerate() {
            *item = (i % 256) as u8;
        }
        let (dtb, virt_base) = dummy_os.alloc_dtb(virt_size, &buf_start);
        let arch = x86::x64::ARCH;
        let translator = x86::x64::new_translator(dtb);

        let cache = PageCache::new(
            arch,
            size::mb(2),
            PageType::PAGE_TABLE | PageType::READ_ONLY,
            TimedCacheValidator::new(Duration::from_secs(100)),
        );

        let mem_cache = CachedPhysicalMemory::new(dummy_os.forward_mut(), cache);
        let mut virt_mem = VirtualDma::new(mem_cache, arch, translator);

        let mut buf_1 = vec![0_u8; 64];
        virt_mem.read_into(virt_base, buf_1.as_mut_slice()).unwrap();

        assert_eq!(buf_start, buf_1);
        buf_1[16..20].copy_from_slice(&[255, 255, 255, 255]);
        virt_mem.write(virt_base + 16_u64, &buf_1[16..20]).unwrap();

        let mut buf_2 = vec![0_u8; 64];
        virt_mem.read_into(virt_base, buf_2.as_mut_slice()).unwrap();

        assert_eq!(buf_1, buf_2);
        assert_ne!(buf_2, buf_start);

        let mut buf_3 = vec![0_u8; 64];

        virt_mem.read_into(virt_base, buf_3.as_mut_slice()).unwrap();
        assert_eq!(buf_2, buf_3);
    }
}

use super::buffer::Buffer;
use super::link;
use std::cell::RefCell;
use std::io::Result;
use std::marker::PhantomData;
use std::mem;
use std::ptr;
use std::rc::Rc;
use std::slice;

const PAGE_SIZE: usize = 4096;
const PAGE_MAP_LEN: usize = PAGE_SIZE / 4;

trait Allocator {
    fn base(&self) -> PagePtr;
    fn allocate(&mut self) -> Option<PagePtr>;
    fn free(&mut self, PagePtr);
}

unsafe fn slice_from_raw_pointer<'a, T>(p: *const u8, bytes: usize) -> &'a [T] {
    slice::from_raw_parts(p as *const T, bytes / mem::size_of::<T>())
}

unsafe fn slice_from_raw_pointer_mut<'a, T>(p: *mut u8, bytes: usize) -> &'a mut [T] {
    slice::from_raw_parts_mut(p as *mut T, bytes / mem::size_of::<T>())
}

#[derive(PartialEq)]
struct PagePtr {
    ptr: *mut u8,
}

impl PagePtr {
    fn new(ptr: *mut u8) -> PagePtr {
        PagePtr { ptr: ptr }
    }

    unsafe fn offset(&self, offset: u32) -> PagePtr {
        let p = self.ptr.offset(((offset as usize) * PAGE_SIZE) as isize);
        PagePtr::new(p)
    }

    unsafe fn calc_offset(&self, p: PagePtr) -> u32 {
        (((p.ptr as usize) - (self.ptr as usize)) / PAGE_SIZE) as u32
    }

    unsafe fn as_slice<'a, T>(self) -> &'a [T] {
        slice_from_raw_pointer(self.ptr, PAGE_SIZE)
    }

    unsafe fn as_slice_mut<'a, T>(self) -> &'a mut [T] {
        slice_from_raw_pointer_mut(self.ptr, PAGE_SIZE)
    }

    unsafe fn raw(self) -> *mut u8 {
        self.ptr
    }
}

#[repr(C)]
struct AllocatedPage {
    lru: link::Link<AllocatedPage>,
    lru_head: *mut link::LinkHead<AllocatedPage>,
    referencer: Rc<RefCell<*mut AllocatedPage>>,
    base: PagePtr,
    data_pages: u32,
    use_count: u32,
}

impl AllocatedPage {
    fn calc_page_count(bytes: usize) -> (usize, usize) {
        // Returns (data count, rel map count)
        let data_pages = if bytes <= AllocatedPage::embed_size() {
            0
        } else {
            (bytes + PAGE_SIZE - 1) / PAGE_SIZE
        };
        let rel_map_pages = if data_pages <= AllocatedPage::embed_map_len() {
            0
        } else {
            (data_pages + PAGE_MAP_LEN - 1) / PAGE_MAP_LEN
        };
        (data_pages, rel_map_pages)
    }

    fn need_pages(bytes: usize) -> usize {
        // Returns needed pages which includes header, rel mapping, and data.
        let (d, m) = AllocatedPage::calc_page_count(bytes);
        d + m + 1
    }

    fn all_pages(&self) -> usize {
        AllocatedPage::need_pages(self.data_pages as usize * PAGE_SIZE)
    }

    unsafe fn allocate_and_set_pages_one<A: Allocator>(map: &mut [u32], allocator: &mut A) {
        for x in map.iter_mut() {
            let page = allocator.allocate().expect("oom");
            *x = allocator.base().calc_offset(page);
        }
    }

    unsafe fn deallocate_pages_one<A: Allocator>(map: &[u32], allocator: &mut A) {
        // deallocate in reverse order to minimize fragmentation.
        let mut i = map.len();
        while i > 0 {
            i -= 1;
            let page = allocator.base().offset(map[i]);
            allocator.free(page);
        }
    }

    unsafe fn allocate<A: Allocator>(
        bytes: usize,
        lru_head: &mut link::LinkHead<AllocatedPage>,
        allocator: &mut A,
    ) -> WeakRefPage {
        // if allocator can not allocate memory, this panics.
        let (data_pages, rel_map_pages) = AllocatedPage::calc_page_count(bytes);
        let map_len = if rel_map_pages > 0 {
            rel_map_pages
        } else {
            data_pages
        };

        let header_p = allocator.allocate().expect("oom").raw() as *mut AllocatedPage;
        let referencer = Rc::new(RefCell::new(header_p));
        let header = header_p.as_mut().unwrap();
        mem::forget(mem::replace(
            header,
            AllocatedPage {
                lru: link::Link::default(),
                lru_head: lru_head,
                referencer: referencer.clone(),
                base: allocator.base(),
                data_pages: data_pages as u32,
                use_count: 0,
            },
        ));
        lru_head.push_front(header.lru());

        // first level
        AllocatedPage::allocate_and_set_pages_one(&mut header.map_mut()[..map_len], allocator);

        // second level
        for i in 0..rel_map_pages {
            let offset = header.map()[i];
            let rel_map = allocator.base().offset(offset).as_slice_mut();
            let rel_map_len = if i + 1 == rel_map_pages && data_pages % PAGE_MAP_LEN > 0 {
                // the last is not fully filled.
                data_pages % PAGE_MAP_LEN
            } else {
                PAGE_MAP_LEN
            };
            AllocatedPage::allocate_and_set_pages_one(&mut rel_map[..rel_map_len], allocator);
        }

        WeakRefPage::new(referencer)
    }

    unsafe fn deallocate<A: Allocator>(raw: *mut AllocatedPage, allocator: &mut A) {
        let header = raw.as_mut().unwrap();
        let (data_pages, rel_map_pages) =
            AllocatedPage::calc_page_count(header.data_pages as usize * PAGE_SIZE);
        let map_len = if rel_map_pages > 0 {
            rel_map_pages
        } else {
            data_pages
        };

        // unlink me
        header.lru().unlink();
        // break reference.
        *header.referencer.borrow_mut() = ptr::null_mut();

        // deallocate pages where rel map refers.
        let mut i = rel_map_pages;
        while i > 0 {
            let rel_map_len = if i == rel_map_pages && data_pages % PAGE_MAP_LEN > 0 {
                // the last map is not fully filled.
                data_pages % PAGE_MAP_LEN
            } else {
                PAGE_MAP_LEN
            };
            i -= 1;
            let rel_map_offset = header.map()[i];
            let rel_map = allocator.base().offset(rel_map_offset).as_slice();
            AllocatedPage::deallocate_pages_one(&rel_map[..rel_map_len], allocator);
        }

        AllocatedPage::deallocate_pages_one(&header.map()[..map_len], allocator);
        mem::drop(mem::replace(header, mem::uninitialized()));
        allocator.free(PagePtr::new(raw as *mut u8));
    }

    fn embed_size() -> usize {
        PAGE_SIZE - mem::size_of::<AllocatedPage>()
    }

    fn embed_map_len() -> usize {
        AllocatedPage::embed_size() / mem::size_of::<u32>()
    }

    unsafe fn embed_as_slice<T>(&self) -> &[T] {
        let p: *const u8 = mem::transmute(self);
        slice_from_raw_pointer(
            p.offset(mem::size_of::<AllocatedPage>() as isize),
            AllocatedPage::embed_size(),
        )
    }

    unsafe fn embed_as_slice_mut<T>(&mut self) -> &mut [T] {
        let p: *mut u8 = mem::transmute(self);
        slice_from_raw_pointer_mut(
            p.offset(mem::size_of::<AllocatedPage>() as isize),
            AllocatedPage::embed_size(),
        )
    }

    unsafe fn map(&self) -> &[u32] {
        self.embed_as_slice()
    }

    unsafe fn map_mut(&mut self) -> &mut [u32] {
        self.embed_as_slice_mut()
    }

    unsafe fn buffer(&mut self) -> &mut [u8] {
        self.embed_as_slice_mut()
    }

    fn lru(&mut self) -> &mut link::Link<AllocatedPage> {
        &mut self.lru
    }

    fn is_embed_page(&self) -> bool {
        self.data_pages == 0
    }

    fn is_relative_using(&self) -> bool {
        self.data_pages > AllocatedPage::embed_map_len() as u32
    }

    fn as_slice_mut(&mut self, n: usize) -> Option<&mut [u8]> {
        if self.is_embed_page() && n == 0 {
            unsafe { Some(self.buffer()) }
        } else if n < self.data_pages as usize {
            let mut n = n as usize;
            let mut map = unsafe { self.map() };
            if self.is_relative_using() {
                let rel_index = n / PAGE_MAP_LEN;
                n = n % PAGE_MAP_LEN;
                map = unsafe { self.base.offset(map[rel_index]).as_slice() };
            }
            unsafe { Some(self.base.offset(map[n]).as_slice_mut()) }
        } else {
            None
        }
    }

    fn inc_use(&mut self) {
        self.use_count += 1;
    }

    fn dec_use(&mut self) {
        self.use_count -= 1;
    }

    fn is_used(&self) -> bool {
        self.use_count > 0
    }

    fn update_lru(&mut self) {
        unsafe {
            self.lru.unlink();
            self.lru_head.as_mut().unwrap().push_front(&mut self.lru);
        }
    }
}

/// FreePage manages continuous pages.
/// This struct aligns tail of pages to minimize allocation cost.
/// | P1 | P2 | ... | PN-1 | FreePage |
#[repr(C)]
struct FreePage {
    link: link::Link<FreePage>,
    count: usize,
}

impl FreePage {
    unsafe fn from_page<'a>(top: PagePtr, count: usize) -> &'a mut FreePage {
        let last = top.offset((count - 1) as u32);
        let p: *mut FreePage = mem::transmute(last.raw());
        let p = p.as_mut().unwrap();
        mem::forget(mem::replace(
            p,
            FreePage {
                link: link::Link::default(),
                count: count,
            },
        ));
        p
    }

    fn link(&mut self) -> &mut link::Link<FreePage> {
        &mut self.link
    }

    unsafe fn reave_page(&mut self) -> PagePtr {
        let top = self.top();
        self.count -= 1;
        if self.count == 0 {
            self.link.unlink();
            mem::drop(mem::replace(self, mem::uninitialized()));
        }
        top
    }

    unsafe fn enlarge(&mut self, count: usize) {
        self.count += count;
    }

    unsafe fn top(&self) -> PagePtr {
        let offset = self.count - 1;
        let p: *mut u8 = mem::transmute(self);
        PagePtr::new(p.offset(-((offset * PAGE_SIZE) as isize)))
    }
}

struct PageAllocator {
    page: Buffer,
    free_list: link::LinkHead<FreePage>,
    free_count: usize,
}

impl PageAllocator {
    fn new(max_pages: usize) -> Result<PageAllocator> {
        let buffer = Buffer::new(max_pages * PAGE_SIZE)?;
        let mut list = link::LinkHead::new();
        unsafe {
            let top = PagePtr::new(buffer.ptr());
            let free_page = FreePage::from_page(top, max_pages);
            list.push_front(free_page.link());
        }
        Ok(PageAllocator {
            page: buffer,
            free_list: list,
            free_count: max_pages,
        })
    }

    fn free_pages(&self) -> usize {
        self.free_count
    }
}

impl Allocator for PageAllocator {
    fn base(&self) -> PagePtr {
        unsafe { PagePtr::new(self.page.ptr()) }
    }

    fn allocate(&mut self) -> Option<PagePtr> {
        if self.free_count == 0 {
            return None;
        }
        self.free_count -= 1;
        unsafe { self.free_list.front_mut().map(|page| page.reave_page()) }
    }

    fn free(&mut self, page: PagePtr) {
        self.free_count += 1;
        unsafe {
            if let Some(front) = self.free_list.front_mut() {
                if page.offset(1) == front.top() {
                    front.enlarge(1);
                    return;
                }
            }
            self.free_list
                .push_front(FreePage::from_page(page, 1).link())
        }
    }
}

pub struct PageManager {
    use_page_lru: link::LinkHead<AllocatedPage>,
    allocator: PageAllocator,
}

impl PageManager {
    pub fn new(max_bytes: usize) -> Result<PageManager> {
        let max_pages = (max_bytes + PAGE_SIZE - 1) / PAGE_SIZE;
        Ok(PageManager {
            use_page_lru: link::LinkHead::new(),
            allocator: PageAllocator::new(max_pages)?,
        })
    }

    pub fn allocate(&mut self, bytes: usize) -> Option<WeakRefPage> {
        let need_pages = AllocatedPage::need_pages(bytes);
        if need_pages > self.allocator.free_pages() {
            let lwm_pages = need_pages - self.allocator.free_pages();
            if !self.free_old_pages(lwm_pages) {
                // oom
                return None;
            }
        }
        unsafe {
            Some(AllocatedPage::allocate(
                bytes,
                &mut self.use_page_lru,
                &mut self.allocator,
            ))
        }
    }

    fn free_old_pages(&mut self, mut lwm_pages: usize) -> bool {
        assert!(lwm_pages > 0);
        for page in self.use_page_lru.iter_reverse_mut() {
            if page.is_used() {
                continue;
            }
            let pages = page.all_pages();
            unsafe {
                AllocatedPage::deallocate(page, &mut self.allocator);
            }
            if pages >= lwm_pages {
                return true;
            }
            lwm_pages -= pages;
        }
        false
    }
}

pub struct WeakRefPage {
    page: Rc<RefCell<*mut AllocatedPage>>,
}

impl WeakRefPage {
    fn new(page: Rc<RefCell<*mut AllocatedPage>>) -> WeakRefPage {
        WeakRefPage { page: page }
    }
    pub fn upgrade(&self) -> Option<RefPage> {
        if self.page.borrow().is_null() {
            None
        } else {
            Some(RefPage::new(self.page.clone()))
        }
    }
}

pub struct RefPage {
    page: Rc<RefCell<*mut AllocatedPage>>,
}

impl RefPage {
    fn new(page: Rc<RefCell<*mut AllocatedPage>>) -> RefPage {
        unsafe {
            page.borrow_mut().as_mut().unwrap().inc_use();
        }
        RefPage { page: page }
    }

    pub fn downgrade(&self) -> WeakRefPage {
        WeakRefPage::new(self.page.clone())
    }

    pub fn get_slices(&self, from: usize) -> SliceIter {
        let page = *self.page.borrow_mut();
        unsafe {
            page.as_mut().unwrap().update_lru();
        }
        SliceIter {
            page: page,
            n: from / PAGE_SIZE,
            offset: from % PAGE_SIZE,
            _m: PhantomData,
        }
    }

    pub fn get_slices_mut(&mut self, from: usize) -> SliceIterMut {
        let page = *self.page.borrow_mut();
        unsafe {
            page.as_mut().unwrap().update_lru();
        }
        SliceIterMut {
            page: page,
            n: from / PAGE_SIZE,
            offset: from % PAGE_SIZE,
            _m: PhantomData,
        }
    }
}

impl Drop for RefPage {
    fn drop(&mut self) {
        unsafe {
            self.page.borrow_mut().as_mut().unwrap().dec_use();
        }
    }
}

pub struct SliceIter<'a>
where
    RefPage: 'a,
{
    page: *mut AllocatedPage,
    n: usize,
    offset: usize,
    _m: PhantomData<&'a RefPage>,
}

impl<'a> Iterator for SliceIter<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        let page = unsafe { self.page.as_mut().unwrap() };
        if let Some(s) = page.as_slice_mut(self.n) {
            let offset = self.offset;
            self.n += 1;
            self.offset = 0;
            Some(&s[offset..])
        } else {
            None
        }
    }
}

pub struct SliceIterMut<'a>
where
    RefPage: 'a,
{
    page: *mut AllocatedPage,
    n: usize,
    offset: usize,
    _m: PhantomData<&'a mut RefPage>,
}

impl<'a> Iterator for SliceIterMut<'a> {
    type Item = &'a mut [u8];
    fn next(&mut self) -> Option<&'a mut [u8]> {
        let page = unsafe { self.page.as_mut().unwrap() };
        if let Some(s) = page.as_slice_mut(self.n) {
            let offset = self.offset;
            self.n += 1;
            self.offset = 0;
            Some(&mut s[offset..])
        } else {
            None
        }
    }
}

#[test]
fn test_iterate() {
    let max = (10 + AllocatedPage::embed_map_len()) * PAGE_SIZE;
    let mut m = PageManager::new(max).unwrap();
    {
        let embed = m.allocate(PAGE_SIZE / 2).unwrap().upgrade().unwrap();
        assert_eq!(embed.get_slices(0).count(), 1);
    }
    {
        let direct = m.allocate(10 * PAGE_SIZE).unwrap().upgrade().unwrap();
        assert_eq!(direct.get_slices(0).count(), 10);
    }
    {
        let relative = m
            .allocate((5 + AllocatedPage::embed_map_len()) * PAGE_SIZE)
            .unwrap()
            .upgrade()
            .unwrap();
        assert_eq!(
            relative.get_slices(0).count(),
            5 + AllocatedPage::embed_map_len()
        );
    }
}

#[test]
fn test_allocate() {
    let mut m = PageManager::new(10 * PAGE_SIZE).unwrap();
    let p1 = m.allocate(1 * PAGE_SIZE);
    let p2 = m.allocate(2 * PAGE_SIZE);
    assert!(p1.is_some());
    assert!(p2.is_some());
    {
        let p1s = p1.as_ref().unwrap().upgrade();
        let p2s = p2.as_ref().unwrap().upgrade();
        assert!(p1s.is_some());
        assert!(p2s.is_some());
        let p3 = m.allocate(9 * PAGE_SIZE);
        assert!(p3.is_none());
    }
    let p4 = m.allocate(9 * PAGE_SIZE);
    assert!(p4.is_some());
    assert!(p1.unwrap().upgrade().is_none());
    assert!(p2.unwrap().upgrade().is_none());
}

#[test]
fn test_ref_page() {
    let magic = [0xd, 0xe, 0xa, 0xd, 0xb, 0xe, 0xe, 0xf];
    let mut m = PageManager::new(10 * PAGE_SIZE).unwrap();
    let p1;
    {
        let p2 = m.allocate(9 * PAGE_SIZE).unwrap();
        let mut p = p2.upgrade().unwrap();
        for s in p.get_slices_mut(0) {
            for (dst, src) in s.iter_mut().zip(magic.iter().cycle()) {
                *dst = *src;
            }
        }
        p1 = p2.upgrade().unwrap();
    }
    for s in p1.get_slices(0) {
        for (x, y) in s.iter().zip(magic.iter().cycle()) {
            assert_eq!(x, y);
        }
    }
}

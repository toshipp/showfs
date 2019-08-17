use std::marker;
use std::mem::transmute;
use std::ptr;

#[repr(C)]
pub struct Link<T> {
    prev: *mut Link<T>,
    next: *mut Link<T>,
}

impl<T> Link<T> {
    pub unsafe fn unlink(&mut self) {
        let prev = self.prev;
        prev.as_mut().unwrap().next = self.next;
        self.next.as_mut().unwrap().prev = prev;
        self.prev = ptr::null_mut();
        self.next = ptr::null_mut();
    }
}

impl<T> Default for Link<T> {
    fn default() -> Link<T> {
        Link::<T> {
            prev: ptr::null_mut(),
            next: ptr::null_mut(),
        }
    }
}

pub struct LinkHead<T> {
    link: Box<Link<T>>,
}

impl<T> LinkHead<T> {
    pub fn new() -> LinkHead<T> {
        let mut link = Box::new(Link::<T>::default());
        link.next = &mut *link;
        link.prev = &mut *link;
        LinkHead { link: link }
    }

    pub fn is_empty(&self) -> bool {
        self.link.next as *const Link<T> == &*self.link
    }

    pub unsafe fn front_mut(&mut self) -> Option<&mut T> {
        if self.is_empty() {
            return None;
        }
        Some(transmute(self.link.next))
    }

    pub unsafe fn push_front(&mut self, element: *mut Link<T>) {
        let next = self.link.next;
        self.link.next = element;
        element.as_mut().unwrap().next = next;
        next.as_mut().unwrap().prev = element;
        element.as_mut().unwrap().prev = &mut *self.link;
    }

    pub fn iter_reverse_mut(&mut self) -> IterReverseMut<T> {
        IterReverseMut {
            link: self.link.prev,
            end: &mut *self.link,
            _m: marker::PhantomData,
        }
    }
}

pub struct IterReverseMut<'a, T>
where
    T: 'a,
{
    link: *mut Link<T>,
    end: *mut Link<T>,
    _m: marker::PhantomData<&'a mut T>,
}

impl<'a, T> Iterator for IterReverseMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<&'a mut T> {
        if self.link == self.end {
            return None;
        }
        let curr = self.link;
        unsafe {
            self.link = self.link.as_mut().unwrap().prev;
            Some(transmute(curr))
        }
    }
}

#[test]
fn test_link() {
    struct Element {
        link: Link<Element>,
        value: usize,
    }
    let mut e1 = Element {
        link: Link::default(),
        value: 0,
    };
    let mut e2 = Element {
        link: Link::default(),
        value: 1,
    };
    let mut e3 = Element {
        link: Link::default(),
        value: 2,
    };

    let mut l = LinkHead::<Element>::new();
    assert!(l.is_empty());

    unsafe {
        l.push_front(&mut e1.link);
        l.push_front(&mut e2.link);
        l.push_front(&mut e3.link);

        assert_eq!(l.front_mut().unwrap().value, 2);

        l.front_mut().unwrap().link.unlink();
        assert_eq!(l.front_mut().unwrap().value, 1);

        use std::vec::Vec;
        let values: Vec<usize> = l.iter_reverse_mut().map(|l| l.value).collect();
        assert_eq!(values, vec![0, 1]);
    }
}

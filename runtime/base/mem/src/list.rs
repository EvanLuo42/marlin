//! Intrusive doubly-linked lists over raw pointers.

/// Defines a list type threading `$node`s through their `$next`/`$prev` fields.
macro_rules! intrusive_list {
    ($(#[$meta:meta])* $name:ident<$node:ty> via $next:ident, $prev:ident) => {
        $(#[$meta])*
        pub(crate) struct $name {
            pub(crate) first: *mut $node,
            last: *mut $node,
        }

        #[allow(dead_code)]
        impl $name {
            pub(crate) const fn new() -> Self {
                Self { first: core::ptr::null_mut(), last: core::ptr::null_mut() }
            }

            /// # Safety
            /// `node` is valid and not in any list using these links.
            pub(crate) unsafe fn push_front(&mut self, node: *mut $node) {
                // SAFETY: `node` and the list's nodes are valid.
                unsafe {
                    (*node).$prev = core::ptr::null_mut();
                    (*node).$next = self.first;
                    if self.first.is_null() {
                        self.last = node;
                    } else {
                        (*self.first).$prev = node;
                    }
                    self.first = node;
                }
            }

            /// # Safety
            /// `node` is valid and not in any list using these links.
            pub(crate) unsafe fn push_back(&mut self, node: *mut $node) {
                // SAFETY: `node` and the list's nodes are valid.
                unsafe {
                    (*node).$next = core::ptr::null_mut();
                    (*node).$prev = self.last;
                    if self.last.is_null() {
                        self.first = node;
                    } else {
                        (*self.last).$next = node;
                    }
                    self.last = node;
                }
            }

            /// # Safety
            /// `node` is in this list.
            pub(crate) unsafe fn remove(&mut self, node: *mut $node) {
                // SAFETY: `node` and its neighbors are valid members of this list.
                unsafe {
                    let next = (*node).$next;
                    let prev = (*node).$prev;
                    if prev.is_null() {
                        self.first = next;
                    } else {
                        (*prev).$next = next;
                    }
                    if next.is_null() {
                        self.last = prev;
                    } else {
                        (*next).$prev = prev;
                    }
                    (*node).$next = core::ptr::null_mut();
                    (*node).$prev = core::ptr::null_mut();
                }
            }

            /// # Safety
            /// `node` is in this list.
            pub(crate) unsafe fn move_to_front(&mut self, node: *mut $node) {
                if self.first != node {
                    // SAFETY: forwarded from the caller.
                    unsafe {
                        self.remove(node);
                        self.push_front(node);
                    }
                }
            }
        }
    };
}

pub(crate) use intrusive_list;

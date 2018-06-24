use std::{intrinsics,ptr,alloc};
use std::sync::{atomic};

pub struct HAlloc<H>(H,atomic::AtomicIsize);

impl<H> HAlloc<H> {
	pub const fn new(h: H) -> HAlloc<H> {
		HAlloc(h,atomic::AtomicIsize::new(isize::min_value()))
	}
	fn init(&self) {
		let mut x = self.1.load(atomic::Ordering::Relaxed);
		if likely(x != isize::min_value() && x != isize::min_value()+1) {
			return;
		}
		if x == isize::min_value() {
			x = self.1.compare_and_swap(isize::min_value(), isize::min_value()+1, atomic::Ordering::Relaxed);
			if x == isize::min_value() {
				let zero = self.1.swap(::internal::heap_size() as isize, atomic::Ordering::Relaxed); if zero != isize::min_value()+1 { unsafe{intrinsics::abort()} }; //assert_eq!(zero, 0);
				return;
			}
		}
		if x == isize::min_value()+1 {
			while self.1.load(atomic::Ordering::Relaxed) == isize::min_value()+1 {
				pause();
			}
		}
	}
}

unsafe impl<H> alloc::GlobalAlloc for HAlloc<H> where H: alloc::GlobalAlloc {
	unsafe fn alloc(&self, l: alloc::Layout) -> *mut alloc::Opaque {
		unimplemented!() // https://github.com/rust-lang/rust/pull/49669
		// self.init();
		// let size = self.0.usable_size(&l).1 as isize;
		// let res = if self.1.fetch_sub(size, atomic::Ordering::Acquire) >= size {
		// 	Ok(self.0.alloc(l)).and_then(|a|if !a.is_null(){Ok(a)}else{Err(alloc::AllocErr)})
		// } else {
		// 	Err(alloc::AllocErr)
		// };
		// if res.is_err() {
		// 	self.1.fetch_add(size, atomic::Ordering::Release);
		// }
		// res.unwrap_or(alloc::Opaque::null_mut())
	}
	unsafe fn dealloc(&self, ptr: *mut alloc::Opaque, layout: alloc::Layout) {
		unimplemented!()
		// <Self as alloc::Alloc>::dealloc(&mut &*self, ptr::NonNull::new(ptr).unwrap(), layout)
	}
	// unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut Opaque {
	// 	let size = layout.size();
	// 	let ptr = self.alloc(layout);
	// 	if !ptr.is_null() {
	// 		ptr::write_bytes(ptr as *mut u8, 0, size);
	// 	}
	// 	ptr
	// }
	// unsafe fn realloc(&self, ptr: *mut Opaque, layout: Layout, new_size: usize) -> *mut Opaque {
	// 	let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
	// 	let new_ptr = self.alloc(new_layout);
	// 	if !new_ptr.is_null() {
	// 		ptr::copy_nonoverlapping(
	// 			ptr as *const u8,
	// 			new_ptr as *mut u8,
	// 			cmp::min(layout.size(), new_size),
	// 		);
	// 		self.dealloc(ptr, layout);
	// 	}
	// 	new_ptr
	// }
}

unsafe impl<H> alloc::Alloc for HAlloc<H> where H: alloc::Alloc {
	unsafe fn alloc(&mut self, l: alloc::Layout) -> Result<ptr::NonNull<alloc::Opaque>, alloc::AllocErr> {
		self.init();
		let size = self.0.usable_size(&l).1 as isize;
		let res = if self.1.fetch_sub(size, atomic::Ordering::Acquire) >= size {
			self.0.alloc(l)
		} else {
			Err(alloc::AllocErr)
		};
		if res.is_err() {
			self.1.fetch_add(size, atomic::Ordering::Release);
		}
		res
	}
	unsafe fn dealloc(&mut self, item: ptr::NonNull<alloc::Opaque>, l: alloc::Layout) {
		self.init();
		let size = self.0.usable_size(&l).1 as isize;
		self.0.dealloc(item, l);
		self.1.fetch_add(size, atomic::Ordering::Release);
	}
	fn usable_size(&self, layout: &alloc::Layout) -> (usize, usize) {
		self.0.usable_size(layout)
	}
	unsafe fn realloc(&mut self, ptr: ptr::NonNull<alloc::Opaque>, old_l: alloc::Layout, new_s: usize) -> Result<ptr::NonNull<alloc::Opaque>, alloc::AllocErr> {
		let new_l = alloc::Layout::from_size_align(new_s, old_l.align()).unwrap();
		self.init();
		let (old_size,new_size) = (self.0.usable_size(&old_l).1 as isize, self.0.usable_size(&new_l).1 as isize);
		if new_size > old_size {
			let res = if self.1.fetch_sub(new_size-old_size, atomic::Ordering::Acquire) >= new_size-old_size {
				self.0.realloc(ptr, old_l, new_s)
			} else {
				Err(alloc::AllocErr)
			};
			if res.is_err() {
				self.1.fetch_add(new_size-old_size, atomic::Ordering::Release);
			}
			res
		} else {
			let res = self.0.realloc(ptr, old_l, new_s);
			if res.is_ok() {
				self.1.fetch_add(old_size-new_size, atomic::Ordering::Release);
			}
			res
		}
	}
	unsafe fn alloc_zeroed(&mut self, l: alloc::Layout) -> Result<ptr::NonNull<alloc::Opaque>, alloc::AllocErr> {
		self.init();
		let size = self.0.usable_size(&l).1 as isize;
		let res = if self.1.fetch_sub(size, atomic::Ordering::Acquire) >= size {
			self.0.alloc_zeroed(l)
		} else {
			Err(alloc::AllocErr)
		};
		if res.is_err() {
			self.1.fetch_add(size, atomic::Ordering::Release);
		}
		res
	}
	unsafe fn grow_in_place(&mut self, ptr: ptr::NonNull<alloc::Opaque>, old_l: alloc::Layout, new_s: usize) -> Result<(), alloc::CannotReallocInPlace> {
		let new_l = alloc::Layout::from_size_align(new_s, old_l.align()).unwrap();
		self.init();
		let (old_size,new_size) = (self.0.usable_size(&old_l).1 as isize, self.0.usable_size(&new_l).1 as isize); if new_size < old_size { intrinsics::abort() };
		let res = if self.1.fetch_sub(new_size-old_size, atomic::Ordering::Acquire) >= new_size-old_size {
			self.0.grow_in_place(ptr, old_l, new_s)
		} else {
			Err(alloc::CannotReallocInPlace)
		};
		if res.is_err() {
			self.1.fetch_add(new_size-old_size, atomic::Ordering::Release);
		}
		res
	}
	unsafe fn shrink_in_place(&mut self, ptr: ptr::NonNull<alloc::Opaque>, old_l: alloc::Layout, new_s: usize) -> Result<(), alloc::CannotReallocInPlace> {
		let new_l = alloc::Layout::from_size_align(new_s, old_l.align()).unwrap();
		self.init();
		let (old_size,new_size) = (self.0.usable_size(&old_l).1 as isize, self.0.usable_size(&new_l).1 as isize); if new_size > old_size { intrinsics::abort() };
		let res = self.0.shrink_in_place(ptr, old_l, new_s);
		if res.is_ok() {
			self.1.fetch_add(old_size-new_size, atomic::Ordering::Release);
		}
		res
	}
}

extern "C" {
	#[link_name = "llvm.x86.sse2.pause"]
	fn pause_();
}
fn pause() {
	unsafe{pause_()};
}
fn likely(b: bool) -> bool {
	unsafe{intrinsics::likely(b)}
}

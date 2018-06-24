use std::{io,marker,ops,fmt,mem};
use fringe;
use bincode;
use serde;
use std::any::Any;
use either::Either;

#[derive(Debug)]
enum SerializerMsg<T> {
	Kill,
	Next,
	New(T),
}
struct SerializerInner<T:serde::ser::Serialize+'static> {
	generator: fringe::generator::Generator<'static,SerializerMsg<T>,Option<u8>,fringe::OsStack>,
	_marker: marker::PhantomData<fn(T)>,
}
unsafe impl<T:serde::ser::Serialize+'static> Send for SerializerInner<T> {} unsafe impl<T:serde::ser::Serialize+'static> Sync for SerializerInner<T> {}
impl<T:serde::ser::Serialize+'static> SerializerInner<T> {
	#[inline(always)]
	fn new() -> SerializerInner<T> {
		let generator = fringe::generator::Generator::<SerializerMsg<T>,Option<u8>,_>::new(fringe::OsStack::new(64*1024).unwrap(), move |yielder, t| {
			let mut x = Some(t);
			while let Some(t) = match x.take().unwrap_or_else(||yielder.suspend(None)) { SerializerMsg::New(t) => Some(t), SerializerMsg::Kill => None, _ => panic!() } {
				if let SerializerMsg::Next = yielder.suspend(None) {} else {panic!()}
				struct Writer<'a,T:'a>(&'a fringe::generator::Yielder<SerializerMsg<T>,Option<u8>>);
				impl<'a,T:'a> io::Write for Writer<'a,T> {
					#[inline(always)]
					fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
						for byte in buf {
							if let SerializerMsg::Next = self.0.suspend(Some(*byte)) {} else {panic!()};
						}
						Ok(buf.len())
					}
					#[inline(always)]
					fn flush(&mut self) -> io::Result<()> { Ok(()) }
				}
				bincode::serialize_into(&mut Writer(yielder), &t).unwrap();
			}
		});
		SerializerInner{generator,_marker:marker::PhantomData}
	}
	#[inline(always)]
	fn push(&mut self, t: T) {
		let x = self.generator.resume(SerializerMsg::New(t)).unwrap(); assert!(x.is_none());
	}
	#[inline(always)]
	fn next(&mut self) -> Option<u8> {
		self.generator.resume(SerializerMsg::Next).unwrap()
	}
}
impl<T:serde::ser::Serialize+'static> ops::Drop for SerializerInner<T> {
	#[inline(always)]
	fn drop(&mut self) {
		let x = self.generator.resume(SerializerMsg::Kill); assert!(x.is_none());
	}
}
trait SerializerInnerBox: Send+Sync {
	fn next_box(&mut self) -> Option<u8>;
	fn as_any_ref(&self) -> &Any;
	fn as_any_mut(&mut self) -> &mut Any;
	fn as_any_box(self: Box<Self>) -> Box<Any>;
}
impl<T:serde::ser::Serialize+'static> SerializerInnerBox for SerializerInner<T> {
	fn next_box(&mut self) -> Option<u8> {
		self.next()
	}
	fn as_any_ref(&self) -> &Any {
		self as &Any
	}
	fn as_any_mut(&mut self) -> &mut Any {
		self as &mut Any
	}
	fn as_any_box(self: Box<Self>) -> Box<Any> {
		self as Box<Any>
	}
}
pub struct Serializer {
	serializer: Option<Box<SerializerInnerBox>>,
	done: bool,
	pull: Option<u8>,
}
impl Serializer {
	pub fn new() -> Serializer {
		Serializer{serializer:None,done:true,pull:None}
	}
	pub fn push_avail(&self) -> bool {
		self.done
	}
	pub fn push<T:serde::ser::Serialize+'static>(&mut self, t: T) {
		assert!(self.done);
		self.done = false;
		if self.serializer.is_none() || !self.serializer.as_ref().unwrap().as_any_ref().is::<SerializerInner<T>>() {
			self.serializer = Some(Box::new(SerializerInner::<T>::new())); // TODO: reuse OsStack
		}
		self.serializer.as_mut().unwrap().as_any_mut().downcast_mut::<SerializerInner<T>>().unwrap().push(t);
		let ret = self.serializer.as_mut().unwrap().next_box();
		if ret.is_none() {
			self.done = true;
		}
		self.pull = ret;
	}
	pub fn pull_avail(&self) -> bool {
		self.pull.is_some()
	}
	pub fn pull(&mut self) -> u8 {
		let ret = self.pull.take().unwrap();
		if !self.done {
			let ret = self.serializer.as_mut().unwrap().next_box();
			if ret.is_none() {
				self.done = true;
			}
			self.pull = ret;
		}
		ret
	}
	pub fn abandon(self) {
		assert!(!self.done || !self.pull.is_none());
		if !self.done {
			mem::forget(self); // HACK!! TODO
		}
	}
}
impl ops::Drop for Serializer {
	fn drop(&mut self) {
		assert!(self.done && self.pull.is_none());
	}
}
impl fmt::Debug for Serializer {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Serializer")
		 .field("done", &self.done)
		 .field("pull", &self.pull.is_some())
		 .finish()
	}
}

#[derive(Debug)]
enum DeserializerMsg {
	Kill,
	Next,
	New(u8),
}
struct DeserializerInner<T:serde::de::DeserializeOwned+'static> {
	generator: fringe::generator::Generator<'static,DeserializerMsg,Either<bool,T>,fringe::OsStack>,
	_marker: marker::PhantomData<fn()->T>,
}
unsafe impl<T:serde::de::DeserializeOwned+'static> Send for DeserializerInner<T> {} unsafe impl<T:serde::de::DeserializeOwned+'static> Sync for DeserializerInner<T> {}
impl<T:serde::de::DeserializeOwned+'static> DeserializerInner<T> {
	#[inline(always)]
	fn new() -> DeserializerInner<T> {
		let generator = fringe::generator::Generator::new(fringe::OsStack::new(64*1024).unwrap(), move |yielder, t| {
			let mut x = Some(t);
			loop {
				let t = match x.take().unwrap_or_else(||yielder.suspend(Either::Left(false))) { DeserializerMsg::New(t) => Some(t), DeserializerMsg::Next => None, DeserializerMsg::Kill => break };
				struct Reader<'a,T:'a>(&'a fringe::generator::Yielder<DeserializerMsg,Either<bool,T>>,Option<u8>);
				impl<'a,T:'a> io::Read for Reader<'a,T> {
					#[inline(always)]
					fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
						for byte in buf.iter_mut() {
							let mut x;
							while {x = self.1.take().or_else(||match self.0.suspend(Either::Left(false)) {DeserializerMsg::New(t) => Some(t), DeserializerMsg::Next => None, _ => panic!()}); x.is_none()} {}
							*byte = x.unwrap();
						}
						Ok(buf.len())
					}
				}
				let ret: T = bincode::deserialize_from(&mut Reader(yielder,t)).unwrap();
				if let DeserializerMsg::Next = yielder.suspend(Either::Left(false)) {} else {panic!()};
				if let DeserializerMsg::Next = yielder.suspend(Either::Left(true)) {} else {panic!()};
				x = Some(yielder.suspend(Either::Right(ret)));
			}
		});
		DeserializerInner{generator,_marker:marker::PhantomData}
	}
	#[inline(always)]
	fn done(&mut self) -> bool {
		self.generator.resume(DeserializerMsg::Next).unwrap().left().unwrap()
	}
	#[inline(always)]
	fn retrieve(&mut self) -> T {
		self.generator.resume(DeserializerMsg::Next).unwrap().right().unwrap()
	}
	// #[inline(always)]
	// fn done(&mut self) -> Option<T> {
	// 	let x = self.done_bool();
	// 	if !x { None } else { Some(self.done_retreive()) }
	// }
	#[inline(always)]
	fn next(&mut self, x: u8) {
		let x = self.generator.resume(DeserializerMsg::New(x)).unwrap(); assert!(!x.left().unwrap());
	}
}
impl<T:serde::de::DeserializeOwned+'static> ops::Drop for DeserializerInner<T> {
	fn drop(&mut self) {
		let x = self.generator.resume(DeserializerMsg::Kill); assert!(x.is_none());
	}
}
trait DeserializerInnerBox: Send+Sync {
	fn next_box(&mut self, x: u8);
	fn done_box(&mut self) -> bool;
	fn as_any_ref(&self) -> &Any;
	fn as_any_mut(&mut self) -> &mut Any;
	fn as_any_box(self: Box<Self>) -> Box<Any>;
}
impl<T:serde::de::DeserializeOwned+'static> DeserializerInnerBox for DeserializerInner<T> {
	fn next_box(&mut self, x: u8) {
		self.next(x)
	}
	fn done_box(&mut self) -> bool {
		self.done()
	}
	fn as_any_ref(&self) -> &Any {
		self as &Any
	}
	fn as_any_mut(&mut self) -> &mut Any {
		self as &mut Any
	}
	fn as_any_box(self: Box<Self>) -> Box<Any> {
		self as Box<Any>
	}
}
pub struct Deserializer {
	deserializer: Option<Box<DeserializerInnerBox>>,
	done: bool,
	pending: bool,
	mid: bool,
}
impl Deserializer {
	pub fn new() -> Deserializer {
		Deserializer{deserializer:None,done:true,pending:false,mid:false}
	}
	pub fn pull_avail(&self) -> bool {
		self.done
	}
	pub fn pull<T:serde::de::DeserializeOwned+'static>(&mut self) {
		assert!(self.done);
		self.done = false;
		if self.deserializer.is_none() || !self.deserializer.as_ref().unwrap().as_any_ref().is::<DeserializerInner<T>>() {
			self.deserializer = Some(Box::new(DeserializerInner::<T>::new()));
		}
		assert!(!self.deserializer.as_mut().unwrap().as_any_mut().downcast_mut::<DeserializerInner<T>>().unwrap().done());
	}
	pub fn mid(&self) -> bool {
		self.mid
	}
	pub fn pull2_avail(&self) -> bool {
		self.pending
	}
	pub fn pull2<T:serde::de::DeserializeOwned+'static>(&mut self) -> T {
		assert!(self.pending);
		self.pending = false;
		self.done = true;
		self.deserializer.as_mut().unwrap().as_any_mut().downcast_mut::<DeserializerInner<T>>().unwrap().retrieve()
	}
	pub fn push_avail(&self) -> bool {
		!self.done && !self.pending
	}
	pub fn push(&mut self, x: u8) {
		assert!(!self.done && !self.pending);
		self.mid = true;
		self.deserializer.as_mut().unwrap().next_box(x);
		if self.deserializer.as_mut().unwrap().done_box() {
			self.mid = false;
			self.pending = true;
		}
	}
	pub fn abandon(self) {
		assert!(!self.done || self.pending);
		mem::forget(self); // HACK!! TODO
	}
}
impl ops::Drop for Deserializer {
	fn drop(&mut self) {
		if let Some(deserializer) = self.deserializer.take() {
			mem::forget(deserializer);
		}
		// assert!(self.done && !self.pending);
	}
}
impl fmt::Debug for Deserializer {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Deserializer")
		 .field("done", &self.done)
		 .field("pending", &self.pending)
		 .field("mid", &self.mid)
		 .finish()
	}
}
use crate::Resources;
use std::{ffi::OsString, net::SocketAddr};

#[derive(Debug)]
pub struct FabricRequest<A, B>
where
	A: FileOrVec,
	B: FileOrVec,
{
	pub resources: Resources,
	pub bind: Vec<SocketAddr>,
	pub args: Vec<OsString>,
	pub vars: Vec<(OsString, OsString)>,
	pub arg: A,
	pub binary: B,
}
pub use fabric_request::{bincode_deserialize_from, FileOrVec};

mod fabric_request {
	use super::FabricRequest;
	use crate::file_from_reader;
	use palaver::file::seal_fd;
	use serde::{
		de::{self, DeserializeSeed, Error, SeqAccess, Visitor}, ser::SerializeTuple, Deserialize, Deserializer, Serialize, Serializer
	};
	use std::{
		cell::UnsafeCell, ffi::OsString, fmt, fs::File, io::{self, Read}, marker::PhantomData, os::unix::io::AsRawFd
	};

	pub trait FileOrVec {
		fn next_element_seed<'de, S, R>(
			self_: &mut S, file_seed: FileSeed<R>,
		) -> Result<Option<Self>, S::Error>
		where
			S: SeqAccess<'de>,
			R: Read,
			Self: Sized;
	}
	impl FileOrVec for File {
		fn next_element_seed<'de, S, R>(
			self_: &mut S, file_seed: FileSeed<R>,
		) -> Result<Option<Self>, S::Error>
		where
			S: SeqAccess<'de>,
			R: Read,
			Self: Sized,
		{
			self_.next_element_seed(file_seed)
		}
	}
	impl FileOrVec for Vec<u8> {
		fn next_element_seed<'de, S, R>(
			self_: &mut S, _file_seed: FileSeed<R>,
		) -> Result<Option<Self>, S::Error>
		where
			S: SeqAccess<'de>,
			R: Read,
			Self: Sized,
		{
			self_.next_element()
		}
	}

	impl Serialize for FabricRequest<Vec<u8>, Vec<u8>> {
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			let mut state = serializer.serialize_tuple(6)?;
			state.serialize_element(&self.resources)?;
			state.serialize_element(&self.bind)?;
			state.serialize_element(&self.args)?;
			state.serialize_element(&self.vars)?;
			state.serialize_element(&self.arg)?;
			state.serialize_element(&self.binary)?;
			state.end()
		}
	}
	impl<'de> Deserialize<'de> for FabricRequest<Vec<u8>, Vec<u8>> {
		fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
		where
			D: Deserializer<'de>,
		{
			deserializer.deserialize_tuple(6, FabricRequestVisitor)
		}
	}
	struct FabricRequestVisitor;
	impl<'de> Visitor<'de> for FabricRequestVisitor {
		type Value = FabricRequest<Vec<u8>, Vec<u8>>;
		fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
			formatter.write_str("a byte array")
		}

		fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
		where
			V: SeqAccess<'de>,
		{
			let resources = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(0, &self))?;
			let bind = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(1, &self))?;
			let args: Vec<OsString> = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(2, &self))?;
			let vars = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(3, &self))?;
			let arg = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			let binary = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(5, &self))?;
			Ok(FabricRequest {
				resources,
				bind,
				args,
				vars,
				arg,
				binary,
			})
		}
	}

	struct FabricRequestSeed<R, A, B> {
		reader: R,
		_marker: PhantomData<fn(A, B)>,
	}
	impl<R, A, B> FabricRequestSeed<R, A, B> {
		fn new(reader: R) -> Self {
			Self {
				reader,
				_marker: PhantomData,
			}
		}
	}
	impl<'de, R, A, B> DeserializeSeed<'de> for FabricRequestSeed<R, A, B>
	where
		R: Read + Copy,
		A: FileOrVec,
		B: FileOrVec,
	{
		type Value = FabricRequest<A, B>;
		fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
		where
			D: Deserializer<'de>,
		{
			deserializer.deserialize_tuple(6, self)
		}
	}
	impl<'de, R, A, B> Visitor<'de> for FabricRequestSeed<R, A, B>
	where
		R: Read + Copy,
		A: FileOrVec,
		B: FileOrVec,
	{
		type Value = FabricRequest<A, B>;
		fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
			formatter.write_str("a byte array")
		}

		fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
		where
			V: SeqAccess<'de>,
		{
			let resources = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(0, &self))?;
			let bind = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(1, &self))?;
			let args: Vec<OsString> = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(2, &self))?;
			let vars = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(3, &self))?;
			let arg = A::next_element_seed(
				&mut seq,
				FileSeed {
					reader: self.reader,
					name: &args[0],
					cloexec: false,
					seal: false,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			let binary = B::next_element_seed(
				&mut seq,
				FileSeed {
					reader: self.reader,
					name: &args[0],
					cloexec: true,
					seal: true,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(5, &self))?;
			Ok(FabricRequest {
				resources,
				bind,
				args,
				vars,
				arg,
				binary,
			})
		}
	}
	#[allow(missing_debug_implementations)]
	pub struct FileSeed<'b, R> {
		reader: R,
		name: &'b OsString,
		cloexec: bool,
		seal: bool,
	}
	impl<'b, 'de, R> DeserializeSeed<'de> for FileSeed<'b, R>
	where
		R: Read,
	{
		type Value = File;
		fn deserialize<D>(mut self, deserializer: D) -> Result<Self::Value, D::Error>
		where
			D: Deserializer<'de>,
		{
			let len: u64 = Deserialize::deserialize(deserializer)?;
			let file = file_from_reader(&mut self.reader, len, self.name, self.cloexec)
				.map_err(Error::custom)?; // TODO we could use specialization to create a proper bincode/whatever io error kind
			if self.seal {
				seal_fd(file.as_raw_fd());
			}
			Ok(file)
		}
	}

	// #[allow(missing_debug_implementations)]
	// struct RefCellReader<R>(RefCell<R>);
	// impl<R> RefCellReader<R> {
	// 	fn new(reader: R) -> Self {
	// 		Self(RefCell::new(reader))
	// 	}
	// }
	// impl<'a, R> Read for &'a RefCellReader<R>
	// where
	// 	R: Read,
	// {
	// 	#[inline(always)]
	// 	fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
	// 		self.0.borrow_mut().read(out)
	// 	}
	// 	#[inline(always)]
	// 	fn read_exact(&mut self, out: &mut [u8]) -> io::Result<()> {
	// 		self.0.borrow_mut().read_exact(out)
	// 	}
	// }
	// impl<'a, 'b, R> bincode::BincodeRead<'a> for &'b RefCellReader<R>
	// where
	// 	R: bincode::BincodeRead<'a>,
	// {
	// 	#[inline(always)]
	// 	fn forward_read_str<V>(&mut self, length: usize, visitor: V) -> bincode::Result<V::Value>
	// 	where
	// 		V: Visitor<'a>,
	// 	{
	// 		self.0.borrow_mut().forward_read_str(length, visitor)
	// 	}
	// 	#[inline(always)]
	// 	fn get_byte_buffer(&mut self, length: usize) -> bincode::Result<Vec<u8>> {
	// 		self.0.borrow_mut().get_byte_buffer(length)
	// 	}
	// 	#[inline(always)]
	// 	fn forward_read_bytes<V>(&mut self, length: usize, visitor: V) -> bincode::Result<V::Value>
	// 	where
	// 		V: Visitor<'a>,
	// 	{
	// 		self.0.borrow_mut().forward_read_bytes(length, visitor)
	// 	}
	// }
	#[allow(missing_debug_implementations)]
	struct UnsafeCellReader<R>(UnsafeCell<R>);
	impl<R> UnsafeCellReader<R> {
		fn new(reader: R) -> Self {
			Self(UnsafeCell::new(reader))
		}
	}
	impl<'a, R> Read for &'a UnsafeCellReader<R>
	where
		R: Read,
	{
		#[inline(always)]
		fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
			unsafe { &mut *self.0.get() }.read(out)
		}
		#[inline(always)]
		fn read_exact(&mut self, out: &mut [u8]) -> io::Result<()> {
			unsafe { &mut *self.0.get() }.read_exact(out)
		}
	}
	impl<'a, 'b, R> bincode::BincodeRead<'a> for &'b UnsafeCellReader<R>
	where
		R: bincode::BincodeRead<'a>,
	{
		#[inline(always)]
		fn forward_read_str<V>(&mut self, length: usize, visitor: V) -> bincode::Result<V::Value>
		where
			V: Visitor<'a>,
		{
			unsafe { &mut *self.0.get() }.forward_read_str(length, visitor)
		}
		#[inline(always)]
		fn get_byte_buffer(&mut self, length: usize) -> bincode::Result<Vec<u8>> {
			unsafe { &mut *self.0.get() }.get_byte_buffer(length)
		}
		#[inline(always)]
		fn forward_read_bytes<V>(&mut self, length: usize, visitor: V) -> bincode::Result<V::Value>
		where
			V: Visitor<'a>,
		{
			unsafe { &mut *self.0.get() }.forward_read_bytes(length, visitor)
		}
	}

	pub fn bincode_deserialize_from<R: Read, A: FileOrVec, B: FileOrVec>(
		stream: &mut R,
	) -> Result<FabricRequest<A, B>, bincode::Error> {
		let reader = UnsafeCellReader::new(stream);
		bincode::config().deserialize_from_seed(FabricRequestSeed::new(&reader), &reader)
	}
}

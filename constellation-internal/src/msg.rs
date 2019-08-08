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
pub use fabric_request::{bincode_deserialize_from, bincode_serialize_into, FileOrVec};

mod fabric_request {
	#![allow(missing_debug_implementations)]

	use super::FabricRequest;
	use crate::file_from_reader;
	use palaver::file::{copy, seal_fd};
	use serde::{
		de::{self, DeserializeSeed, SeqAccess, Visitor}, ser::{self, SerializeTuple}, Deserialize, Deserializer, Serialize, Serializer
	};
	use std::{
		cell::UnsafeCell, ffi::OsString, fmt, fs::File, io::{self, Read, Write}, marker::PhantomData, os::unix::io::AsRawFd
	};

	pub trait FileOrVec {
		// type Serializer: Serialize + ?Sized;
		fn next_element_seed<'de, S, R>(
			self_: &mut S, file_seed: FileSeed<R>,
		) -> Result<Option<Self>, S::Error>
		where
			S: SeqAccess<'de>,
			R: Read,
			Self: Sized;
		fn as_serializer<'a, W: Write>(
			&'a self, writer: &'a UnsafeCell<W>,
		) -> PoorGat<FileSerializer<'a, W>, &'a serde_bytes::Bytes>;
	}
	impl FileOrVec for File {
		// type Serializer<'a, W> = FileSerializer<'a, W>; // TODO: When GAT arrives
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
		fn as_serializer<'a, W: Write>(
			&'a self, writer: &'a UnsafeCell<W>,
		) -> PoorGat<FileSerializer<'a, W>, &'a serde_bytes::Bytes> {
			PoorGat::File(FileSerializer(self, writer))
		}
	}
	impl FileOrVec for Vec<u8> {
		// type Serializer = serde_bytes::Bytes;
		fn next_element_seed<'de, S, R>(
			self_: &mut S, _file_seed: FileSeed<R>,
		) -> Result<Option<Self>, S::Error>
		where
			S: SeqAccess<'de>,
			R: Read,
			Self: Sized,
		{
			self_
				.next_element::<serde_bytes::ByteBuf>()
				.map(|x| x.map(serde_bytes::ByteBuf::into_vec))
		}
		fn as_serializer<'a, W: Write>(
			&'a self, _writer: &'a UnsafeCell<W>,
		) -> PoorGat<FileSerializer<'a, W>, &'a serde_bytes::Bytes> {
			PoorGat::Vec(serde_bytes::Bytes::new(self))
		}
	}
	pub enum PoorGat<A, B> {
		File(A),
		Vec(B),
	}
	pub struct FileSerializer<'a, W: Write>(&'a File, &'a UnsafeCell<W>);
	impl<'a, W: Write> Serialize for FileSerializer<'a, W> {
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			let len: u64 = self.0.metadata().unwrap().len();
			let ok = len.serialize(serializer)?;
			copy(&mut &*self.0, unsafe { &mut *self.1.get() }, len).map_err(ser::Error::custom)?;
			// copy_sendfile(&*self.0, unsafe { &mut *self.1.get() }, len).map_err(ser::Error::custom)?; // TODO
			Ok(ok)
		}
	}
	impl<'a, W: Write> Serialize for PoorGat<FileSerializer<'a, W>, &'a serde_bytes::Bytes> {
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			match self {
				PoorGat::Vec(left) => left.serialize(serializer),
				PoorGat::File(right) => right.serialize(serializer),
			}
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
			state.serialize_element(&serde_bytes::Bytes::new(&self.arg))?;
			state.serialize_element(&serde_bytes::Bytes::new(&self.binary))?;
			state.end()
		}
	}
	struct FabricRequestSerializer<'a, W, A, B>
	where
		W: Write,
		A: FileOrVec,
		B: FileOrVec,
	{
		writer: UnsafeCell<W>,
		value: &'a FabricRequest<A, B>,
	}
	impl<'a, W, A, B> FabricRequestSerializer<'a, W, A, B>
	where
		W: Write,
		A: FileOrVec,
		B: FileOrVec,
	{
		fn new(writer: W, value: &'a FabricRequest<A, B>) -> Self {
			FabricRequestSerializer {
				writer: UnsafeCell::new(writer),
				value,
			}
		}
	}
	impl<'a, W, A, B> Serialize for FabricRequestSerializer<'a, W, A, B>
	where
		W: Write,
		A: FileOrVec,
		B: FileOrVec,
	{
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			let mut state = serializer.serialize_tuple(6)?;
			state.serialize_element(&self.value.resources)?;
			state.serialize_element(&self.value.bind)?;
			state.serialize_element(&self.value.args)?;
			state.serialize_element(&self.value.vars)?;
			state.serialize_element(&self.value.arg.as_serializer(&self.writer))?;
			state.serialize_element(&self.value.binary.as_serializer(&self.writer))?;
			state.end()
		}
	}

	// impl<'de> Deserialize<'de> for FabricRequest<Vec<u8>, Vec<u8>> {
	// 	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	// 	where
	// 		D: Deserializer<'de>,
	// 	{
	// 		deserializer.deserialize_tuple(6, FabricRequestVisitor)
	// 	}
	// }
	// struct FabricRequestVisitor;
	// impl<'de> Visitor<'de> for FabricRequestVisitor {
	// 	type Value = FabricRequest<Vec<u8>, Vec<u8>>;
	// 	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
	// 		formatter.write_str("a byte array")
	// 	}

	// 	fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
	// 	where
	// 		V: SeqAccess<'de>,
	// 	{
	// 		let resources = seq
	// 			.next_element()?
	// 			.ok_or_else(|| de::Error::invalid_length(0, &self))?;
	// 		let bind = seq
	// 			.next_element()?
	// 			.ok_or_else(|| de::Error::invalid_length(1, &self))?;
	// 		let args: Vec<OsString> = seq
	// 			.next_element()?
	// 			.ok_or_else(|| de::Error::invalid_length(2, &self))?;
	// 		let vars = seq
	// 			.next_element()?
	// 			.ok_or_else(|| de::Error::invalid_length(3, &self))?;
	// 		let arg = seq
	// 			.next_element::<serde_bytes::ByteBuf>()?
	// 			.ok_or_else(|| de::Error::invalid_length(4, &self))?
	// 			.into_vec();
	// 		let binary = seq
	// 			.next_element::<serde_bytes::ByteBuf>()?
	// 			.ok_or_else(|| de::Error::invalid_length(5, &self))?
	// 			.into_vec();
	// 		Ok(FabricRequest {
	// 			resources,
	// 			bind,
	// 			args,
	// 			vars,
	// 			arg,
	// 			binary,
	// 		})
	// 	}
	// }

	thread_local!(static READER: std::cell::RefCell<Option<*mut dyn Read>> = std::cell::RefCell::new(None));

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
	impl<'de, A, B> Deserialize<'de> for FabricRequest<A, B>
	where
		A: FileOrVec,
		B: FileOrVec,
	{
		fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
		where
			D: Deserializer<'de>,
		{
			READER.with(|reader| {
				deserializer.deserialize_tuple(
					6,
					FabricRequestSeed::new(unsafe { &mut **reader.borrow_mut().as_mut().unwrap() }),
				)
			})
		}
	}
	impl<'de, R, A, B> Visitor<'de> for FabricRequestSeed<R, A, B>
	where
		R: Read,
		A: FileOrVec,
		B: FileOrVec,
	{
		type Value = FabricRequest<A, B>;
		fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
			formatter.write_str("a byte array")
		}

		fn visit_seq<V>(mut self, mut seq: V) -> Result<Self::Value, V::Error>
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
					reader: &mut self.reader,
					name: &args[0],
					cloexec: false,
					seal: false,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			let binary = B::next_element_seed(
				&mut seq,
				FileSeed {
					reader: &mut self.reader,
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
				.map_err(de::Error::custom)?; // TODO we could use specialization to create a proper bincode/whatever io error kind
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
	#[allow(missing_debug_implementations)]
	struct UnsafeCellReaderWriter<R>(UnsafeCell<R>);
	impl<R> UnsafeCellReaderWriter<R> {
		fn new(reader: R) -> Self {
			Self(UnsafeCell::new(reader))
		}
	}
	impl<'a, R> Read for &'a UnsafeCellReaderWriter<R>
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
	impl<'a, W> Write for &'a UnsafeCellReaderWriter<W>
	where
		W: Write,
	{
		#[inline(always)]
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
			unsafe { &mut *self.0.get() }.write(buf)
		}
		#[inline(always)]
		fn flush(&mut self) -> io::Result<()> {
			unsafe { &mut *self.0.get() }.flush()
		}
		#[inline(always)]
		fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
			unsafe { &mut *self.0.get() }.write_all(buf)
		}
	}
	pub fn bincode_deserialize_from<R: Read, A: FileOrVec, B: FileOrVec>(
		stream: &mut R,
	) -> Result<FabricRequest<A, B>, bincode::Error> {
		let reader = UnsafeCellReaderWriter::new(stream);
		READER.with(|reader_| {
			let to: *mut dyn Read = &mut &reader;
			#[allow(clippy::transmute_ptr_to_ptr)]
			let to: *mut dyn Read = unsafe { std::mem::transmute(to) };
			*reader_.borrow_mut() = Some(to);
			let ret = bincode::config().deserialize_from(&reader);
			let _ = reader_.borrow_mut().take().unwrap();
			ret
		})
	}
	pub fn bincode_serialize_into<W: Write, A: FileOrVec, B: FileOrVec>(
		stream: &mut W, value: &FabricRequest<A, B>,
	) -> Result<(), bincode::Error> {
		let writer = UnsafeCellReaderWriter::new(stream);
		bincode::config().serialize_into(&writer, &FabricRequestSerializer::new(&writer, value))
	}
}

use crate::Resources;
#[cfg(not(feature = "distribute_binaries"))]
use std::marker::PhantomData;
use std::{ffi::OsString, net::SocketAddr};

#[derive(Debug)]
pub struct FabricRequest<A, B>
where
	A: FileOrVec,
	B: FileOrVec,
{
	/// Whether to wait for space to allocate the process, or just bail.
	pub block: bool,
	/// The resources required for this process.
	pub resources: Resources,
	/// The socket addresses required to bind to for this process.
	pub bind: Vec<SocketAddr>,
	/// The command line arguments passed to the process.
	pub args: Vec<OsString>,
	/// The environment variables passed to the process.
	pub vars: Vec<(OsString, OsString)>,
	/// An extra argument passed to the process on a special file descriptor.
	pub arg: A,
	/// The process to spawn.
	#[cfg(feature = "distribute_binaries")]
	pub binary: B,
	#[cfg(not(feature = "distribute_binaries"))]
	pub binary: PhantomData<B>,
}

/// This is the request made by `deploy` to the `bridge`.
#[derive(Debug)]
pub struct BridgeRequest<A, B>
where
	A: FileOrVec,
	B: FileOrVec,
{
	/// The resources required for this process. `None` here means the bridge does a recce to determine what's passed to `init()`.
	pub resources: Option<Resources>,
	/// The command line arguments passed to the process.
	pub args: Vec<OsString>,
	/// The environment variables passed to the process.
	pub vars: Vec<(OsString, OsString)>,
	/// An extra argument passed to the process on a special file descriptor.
	pub arg: A,
	/// The process to spawn.
	#[cfg(feature = "distribute_binaries")]
	pub binary: B,
	#[cfg(not(feature = "distribute_binaries"))]
	pub binary: PhantomData<B>,
}

pub use self::serde::{bincode_deserialize_from, bincode_serialize_into, FileOrVec};

mod serde {
	#![allow(missing_debug_implementations)]

	use super::{BridgeRequest, FabricRequest};
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
				Self::Vec(left) => left.serialize(serializer),
				Self::File(right) => right.serialize(serializer),
			}
		}
	}

	impl Serialize for FabricRequest<Vec<u8>, Vec<u8>> {
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			let mut state = serializer.serialize_tuple(7)?;
			state.serialize_element(&self.block)?;
			state.serialize_element(&self.resources)?;
			state.serialize_element(&self.bind)?;
			state.serialize_element(&self.args)?;
			state.serialize_element(&self.vars)?;
			state.serialize_element(&serde_bytes::Bytes::new(&self.arg))?;
			#[cfg(feature = "distribute_binaries")]
			state.serialize_element(&serde_bytes::Bytes::new(&self.binary))?;
			#[cfg(not(feature = "distribute_binaries"))]
			state.serialize_element(&PhantomData::<Vec<u8>>)?;
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
			let mut state = serializer.serialize_tuple(7)?;
			state.serialize_element(&self.value.block)?;
			state.serialize_element(&self.value.resources)?;
			state.serialize_element(&self.value.bind)?;
			state.serialize_element(&self.value.args)?;
			state.serialize_element(&self.value.vars)?;
			state.serialize_element(&self.value.arg.as_serializer(&self.writer))?;
			#[cfg(feature = "distribute_binaries")]
			state.serialize_element(&self.value.binary.as_serializer(&self.writer))?;
			#[cfg(not(feature = "distribute_binaries"))]
			state.serialize_element(&PhantomData::<B>)?;
			state.end()
		}
	}

	impl Serialize for BridgeRequest<Vec<u8>, Vec<u8>> {
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			let mut state = serializer.serialize_tuple(6)?;
			state.serialize_element(&self.resources)?;
			state.serialize_element(&self.args)?;
			state.serialize_element(&self.vars)?;
			state.serialize_element(&serde_bytes::Bytes::new(&self.arg))?;
			#[cfg(feature = "distribute_binaries")]
			state.serialize_element(&serde_bytes::Bytes::new(&self.binary))?;
			#[cfg(not(feature = "distribute_binaries"))]
			state.serialize_element(&PhantomData::<Vec<u8>>)?;
			state.end()
		}
	}
	struct BridgeRequestSerializer<'a, W, A, B>
	where
		W: Write,
		A: FileOrVec,
		B: FileOrVec,
	{
		writer: UnsafeCell<W>,
		value: &'a BridgeRequest<A, B>,
	}
	impl<'a, W, A, B> BridgeRequestSerializer<'a, W, A, B>
	where
		W: Write,
		A: FileOrVec,
		B: FileOrVec,
	{
		fn new(writer: W, value: &'a BridgeRequest<A, B>) -> Self {
			BridgeRequestSerializer {
				writer: UnsafeCell::new(writer),
				value,
			}
		}
	}
	impl<'a, W, A, B> Serialize for BridgeRequestSerializer<'a, W, A, B>
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
			state.serialize_element(&self.value.args)?;
			state.serialize_element(&self.value.vars)?;
			state.serialize_element(&self.value.arg.as_serializer(&self.writer))?;
			#[cfg(feature = "distribute_binaries")]
			state.serialize_element(&self.value.binary.as_serializer(&self.writer))?;
			#[cfg(not(feature = "distribute_binaries"))]
			state.serialize_element(&PhantomData::<B>)?;
			state.end()
		}
	}

	impl<'de> Deserialize<'de> for FabricRequest<Vec<u8>, Vec<u8>> {
		fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
		where
			D: Deserializer<'de>,
		{
			deserializer.deserialize_tuple(7, FabricRequestVisitor)
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
			let block = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(0, &self))?;
			let resources = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(1, &self))?;
			let bind = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(2, &self))?;
			let args: Vec<OsString> = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(3, &self))?;
			let vars = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			let arg = seq
				.next_element::<serde_bytes::ByteBuf>()?
				.ok_or_else(|| de::Error::invalid_length(5, &self))?
				.into_vec();
			let binary = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(6, &self))?;
			#[cfg(feature = "distribute_binaries")]
			let binary = serde_bytes::ByteBuf::into_vec(binary);
			Ok(FabricRequest {
				block,
				resources,
				bind,
				args,
				vars,
				arg,
				binary,
			})
		}
	}

	impl<'de> Deserialize<'de> for BridgeRequest<Vec<u8>, Vec<u8>> {
		fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
		where
			D: Deserializer<'de>,
		{
			deserializer.deserialize_tuple(6, BridgeRequestVisitor)
		}
	}
	struct BridgeRequestVisitor;
	impl<'de> Visitor<'de> for BridgeRequestVisitor {
		type Value = BridgeRequest<Vec<u8>, Vec<u8>>;
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
			let args: Vec<OsString> = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(1, &self))?;
			let vars = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(2, &self))?;
			let arg = seq
				.next_element::<serde_bytes::ByteBuf>()?
				.ok_or_else(|| de::Error::invalid_length(3, &self))?
				.into_vec();
			let binary = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			#[cfg(feature = "distribute_binaries")]
			let binary = serde_bytes::ByteBuf::into_vec(binary);
			Ok(BridgeRequest {
				resources,
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
		R: Read,
		A: FileOrVec,
		B: FileOrVec,
	{
		type Value = FabricRequest<A, B>;

		fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
		where
			D: Deserializer<'de>,
		{
			deserializer.deserialize_tuple(7, self)
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
			let block = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(0, &self))?;
			let resources = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(1, &self))?;
			let bind = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(2, &self))?;
			let args: Vec<OsString> = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(3, &self))?;
			let vars = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			let arg = A::next_element_seed(
				&mut seq,
				FileSeed {
					reader: &mut self.reader,
					name: &args[0],
					cloexec: false,
					seal: false,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(5, &self))?;
			#[cfg(feature = "distribute_binaries")]
			let binary = B::next_element_seed(
				&mut seq,
				FileSeed {
					reader: &mut self.reader,
					name: &args[0],
					cloexec: true,
					seal: true,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(6, &self))?;
			#[cfg(not(feature = "distribute_binaries"))]
			let binary = seq.next_element()?
				.ok_or_else(|| de::Error::invalid_length(6, &self))?;
			Ok(FabricRequest {
				block,
				resources,
				bind,
				args,
				vars,
				arg,
				binary,
			})
		}
	}

	struct BridgeRequestSeed<R, A, B> {
		reader: R,
		_marker: PhantomData<fn(A, B)>,
	}
	impl<R, A, B> BridgeRequestSeed<R, A, B> {
		fn new(reader: R) -> Self {
			Self {
				reader,
				_marker: PhantomData,
			}
		}
	}
	impl<'de, R, A, B> DeserializeSeed<'de> for BridgeRequestSeed<R, A, B>
	where
		R: Read,
		A: FileOrVec,
		B: FileOrVec,
	{
		type Value = BridgeRequest<A, B>;

		fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
		where
			D: Deserializer<'de>,
		{
			deserializer.deserialize_tuple(6, self)
		}
	}
	impl<'de, R, A, B> Visitor<'de> for BridgeRequestSeed<R, A, B>
	where
		R: Read,
		A: FileOrVec,
		B: FileOrVec,
	{
		type Value = BridgeRequest<A, B>;
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
			let args: Vec<OsString> = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(1, &self))?;
			let vars = seq
				.next_element()?
				.ok_or_else(|| de::Error::invalid_length(2, &self))?;
			let arg = A::next_element_seed(
				&mut seq,
				FileSeed {
					reader: &mut self.reader,
					name: &args[0],
					cloexec: false,
					seal: false,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(3, &self))?;
			#[cfg(feature = "distribute_binaries")]
			let binary = B::next_element_seed(
				&mut seq,
				FileSeed {
					reader: &mut self.reader,
					name: &args[0],
					cloexec: true,
					seal: true,
				},
			)?
			.ok_or_else(|| de::Error::invalid_length(4, &self))?;
			#[cfg(not(feature = "distribute_binaries"))]
			let binary = seq.next_element()?
				.ok_or_else(|| de::Error::invalid_length(5, &self))?;
			Ok(BridgeRequest {
				resources,
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
	pub fn bincode_deserialize_from<R: Read, D: BincodeDeserializeFrom>(
		stream: &mut R,
	) -> Result<D, bincode::Error> {
		D::bincode_deserialize_from(stream)
	}
	pub trait BincodeDeserializeFrom {
		fn bincode_deserialize_from<R: Read>(stream: &mut R) -> Result<Self, bincode::Error>
		where
			Self: Sized;
	}
	impl<A: FileOrVec, B: FileOrVec> BincodeDeserializeFrom for FabricRequest<A, B> {
		fn bincode_deserialize_from<R: Read>(stream: &mut R) -> Result<Self, bincode::Error> {
			let reader = UnsafeCellReaderWriter::new(stream);
			bincode::config().deserialize_from_seed(FabricRequestSeed::new(&reader), &reader)
		}
	}
	impl<A: FileOrVec, B: FileOrVec> BincodeDeserializeFrom for BridgeRequest<A, B> {
		fn bincode_deserialize_from<R: Read>(stream: &mut R) -> Result<Self, bincode::Error> {
			let reader = UnsafeCellReaderWriter::new(stream);
			bincode::config().deserialize_from_seed(BridgeRequestSeed::new(&reader), &reader)
		}
	}
	pub fn bincode_serialize_into<W: Write, S: BincodeSerializeInto>(
		stream: &mut W, value: &S,
	) -> Result<(), bincode::Error> {
		S::bincode_serialize_into(stream, value)
	}
	pub trait BincodeSerializeInto {
		fn bincode_serialize_into<W: Write>(
			stream: &mut W, value: &Self,
		) -> Result<(), bincode::Error>
		where
			Self: Sized;
	}
	impl<A: FileOrVec, B: FileOrVec> BincodeSerializeInto for FabricRequest<A, B> {
		fn bincode_serialize_into<W: Write>(
			stream: &mut W, value: &Self,
		) -> Result<(), bincode::Error> {
			let writer = UnsafeCellReaderWriter::new(stream);
			bincode::config().serialize_into(&writer, &FabricRequestSerializer::new(&writer, value))
		}
	}
	impl<A: FileOrVec, B: FileOrVec> BincodeSerializeInto for BridgeRequest<A, B> {
		fn bincode_serialize_into<W: Write>(
			stream: &mut W, value: &Self,
		) -> Result<(), bincode::Error> {
			let writer = UnsafeCellReaderWriter::new(stream);
			bincode::config().serialize_into(&writer, &BridgeRequestSerializer::new(&writer, value))
		}
	}
}

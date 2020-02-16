mod owningorref {
	use serde::{de::Deserializer, ser::Serializer, Deserialize, Serialize};
	use std::{
		fmt::{self, Debug}, ops::Deref
	};

	pub enum OwningOrRef<'a, T>
	where
		T: Deref,
	{
		Owning(T),
		Ref(&'a T::Target),
	}

	impl<'a, T> OwningOrRef<'a, T>
	where
		T: Deref,
	{
		pub fn into_inner(self) -> Option<T> {
			match self {
				Self::Owning(a) => Some(a),
				Self::Ref(_) => None,
			}
		}
	}

	impl<'a, T> Clone for OwningOrRef<'a, T>
	where
		T: Deref + Clone,
	{
		fn clone(&self) -> Self {
			match self {
				Self::Owning(a) => Self::Owning(a.clone()),
				Self::Ref(a) => Self::Ref(Clone::clone(a)),
			}
		}
	}
	impl<'a, T> Copy for OwningOrRef<'a, T> where T: Deref + Copy {}

	impl<'a, T> Debug for OwningOrRef<'a, T>
	where
		T: Deref + Debug,
		T::Target: Debug,
	{
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			match self {
				Self::Owning(a) => a.fmt(f),
				Self::Ref(a) => a.fmt(f),
			}
		}
	}

	impl<'a, T> Deref for OwningOrRef<'a, T>
	where
		T: Deref,
	{
		type Target = T::Target;

		fn deref(&self) -> &Self::Target {
			match self {
				Self::Owning(a) => &**a,
				Self::Ref(a) => a,
			}
		}
	}

	impl<'a, T> Serialize for OwningOrRef<'a, T>
	where
		T: Deref,
		T::Target: Serialize,
	{
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			(**self).serialize(serializer)
		}
	}
	impl<'de, 'a, T> Deserialize<'de> for OwningOrRef<'a, T>
	where
		T: Deref + Deserialize<'de>,
	{
		fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
		where
			D: Deserializer<'de>,
		{
			T::deserialize(deserializer).map(Self::Owning)
		}
	}
}
pub use self::owningorref::OwningOrRef;

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

mod bufferedstream {
	use std::io::{self, Read, Write};
	#[derive(Debug)]
	pub struct BufferedStream<T: Read + Write> {
		stream: io::BufReader<T>,
	}
	impl<T: Read + Write> BufferedStream<T> {
		pub fn new(stream: T) -> Self {
			Self {
				stream: io::BufReader::new(stream),
			}
		}

		pub fn write(&mut self) -> BufferedStreamWriter<T> {
			BufferedStreamWriter(io::BufWriter::new(Wrap(self)))
		}

		pub fn get_ref(&self) -> &T {
			self.stream.get_ref()
		}

		pub fn get_mut(&mut self) -> &mut T {
			self.stream.get_mut()
		}
	}
	impl<T: Read + Write> Read for BufferedStream<T> {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			self.stream.read(buf)
		}
	}
	#[derive(Debug)]
	struct Wrap<T>(T);
	impl<'a, T: Read + Write> Write for Wrap<&'a mut BufferedStream<T>> {
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
			self.0.stream.get_mut().write(buf)
		}

		fn flush(&mut self) -> io::Result<()> {
			self.0.stream.get_mut().flush()
		}
	}
	#[derive(Debug)]
	pub struct BufferedStreamWriter<'a, T: Read + Write>(
		io::BufWriter<Wrap<&'a mut BufferedStream<T>>>,
	);
	impl<'a, T: Read + Write> Write for BufferedStreamWriter<'a, T> {
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
			self.0.write(buf)
		}

		fn flush(&mut self) -> io::Result<()> {
			self.0.flush()
		}
	}
	impl<'a, T: Read + Write> Drop for BufferedStreamWriter<'a, T> {
		fn drop(&mut self) {
			self.0.flush().unwrap();
		}
	}
}
pub use self::bufferedstream::BufferedStream;

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

mod to_hex {
	use std::fmt;
	#[derive(Clone, Debug)]
	pub struct Hex<'a>(&'a [u8], bool);
	impl<'a> Iterator for Hex<'a> {
		type Item = char;

		fn next(&mut self) -> Option<char> {
			if !self.0.is_empty() {
				const CHARS: &[u8] = b"0123456789abcdef";
				let byte = self.0[0];
				let second = self.1;
				if second {
					self.0 = self.0.split_first().unwrap().1;
				}
				self.1 = !self.1;
				Some(CHARS[if !second { byte >> 4 } else { byte & 0xf } as usize] as char)
			} else {
				None
			}
		}
	}
	impl<'a> fmt::Display for Hex<'a> {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			for char_ in self.clone() {
				write!(f, "{}", char_)?;
			}
			Ok(())
		}
	}
	pub trait ToHex {
		fn to_hex(&self) -> Hex;
	}
	impl ToHex for [u8] {
		fn to_hex(&self) -> Hex {
			Hex(&*self, false)
		}
	}
}
pub use self::to_hex::ToHex;

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

mod rand_stream {
	#[derive(Debug)]
	pub struct Rand<T> {
		res: Option<T>,
		count: usize,
	}
	impl<T> Rand<T> {
		pub fn new() -> Self {
			Self {
				res: None,
				count: 0,
			}
		}

		pub fn push<R: rand::Rng>(&mut self, x: T, rng: &mut R) {
			self.count += 1;
			if rng.gen_range(0, self.count) == 0 {
				self.res = Some(x);
			}
		}

		pub fn get(self) -> Option<T> {
			self.res
		}
	}
	impl<T> Default for Rand<T> {
		fn default() -> Self {
			Self::new()
		}
	}
}
pub use self::rand_stream::Rand;

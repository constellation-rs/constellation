pub mod serialize_as_regex_string {
	use std::{fmt,char};
	use std::fmt::Write;
	use serde::{Serializer, de, Deserialize, Deserializer};
	use regex;

	#[derive(PartialEq,Eq,Hash,Serialize,Debug)]
	pub struct SerializeAsRegexString(#[serde(with = "self")] pub Vec<u8>);

	pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
		serializer.collect_str(&regex::escape(&String::from_utf8_lossy(bytes))) // TODO: escape rather than lose invalid chars
	}
}

pub mod serde_regex { // https://github.com/tailhook/serde-regex
	use std::{fmt,hash};
	use regex;
	use regex::bytes::{Regex,RegexBuilder};
	use serde::de::{Visitor, Error};
	use serde::{Deserializer, Serializer};

	#[derive(Serialize,Deserialize,Debug)]
	pub struct SerdeRegex(#[serde(with = "self")] regex::bytes::Regex);
	impl SerdeRegex {
		pub fn is_match(&self, text: &[u8]) -> bool {
			self.0.is_match(text)
		}
	}
	impl PartialEq for SerdeRegex {
		fn eq(&self, other: &Self) -> bool {
			self.0.as_str() == other.0.as_str()
		}
	}
	impl Eq for SerdeRegex {}
	impl hash::Hash for SerdeRegex {
		fn hash<H>(&self, state: &mut H) where H: hash::Hasher {
			self.0.as_str().hash(state);
		}
	}

	struct RegexVisitor;
	impl<'a> Visitor<'a> for RegexVisitor {
		type Value = Regex;
		fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
			formatter.write_str("valid regular expression")
		}
		fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> where E: Error {
			RegexBuilder::new(&format!("^{}$", value)).unicode(false).dot_matches_new_line(true).build().map_err(E::custom)
		}
	}
	pub fn serialize<S>(value: &Regex, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
		let x = value.as_str();
		assert_eq!(x.chars().nth(0).unwrap(), '^');
		assert_eq!(x.chars().rev().nth(0).unwrap(), '$');
		serializer.serialize_str(&x[1..x.len()-1])
	}
	pub fn deserialize<'de, D>(deserializer: D) -> Result<Regex, D::Error> where D: Deserializer<'de>, {
		deserializer.deserialize_str(RegexVisitor)
	}
}

pub mod string { // Until there is an into_chars() in stdlib
	use std::{str,mem};

	pub struct Chars(String,str::Chars<'static>);
	impl Chars {
		pub fn new(s: String) -> Chars {
			let x = unsafe{mem::transmute::<&str,&str>(&*s)}.chars();
			Chars(s, x)
		}
	}
	impl Iterator for Chars {
		type Item = char;
		fn next(&mut self) -> Option<char> {
			self.1.next()
		}
	}
}

pub mod hashmap {
	use std::{hash};
	use std::collections::hash_map;
	use std::collections::hash_map::HashMap;

	pub fn intersection<'a,K:'a+Eq+hash::Hash,V1:'a,V2:'a,S:'a+hash::BuildHasher>(self_: &'a HashMap<K,V1,S>, other: &'a HashMap<K,V2,S>) -> Intersection<'a, K, V1, V2, S> {
		Intersection {
			iter: self_.iter(),
			other,
		}
	}
	pub struct Intersection<'a,K:'a,V1:'a,V2:'a,S:'a> {
		iter: hash_map::Iter<'a, K, V1>,
		other: &'a HashMap<K, V2, S>,
	}
	impl<'a, K, V1, V2, S> Iterator for Intersection<'a, K, V1, V2, S>
		where K: Eq + hash::Hash,
			  S: hash::BuildHasher
	{
		type Item = (&'a K,&'a V1,&'a V2);
		fn next(&mut self) -> Option<(&'a K,&'a V1,&'a V2)> {
			loop {
				let elt = self.iter.next()?;
				if let Some(elt2) = self.other.get(elt.0) {
					return Some((elt.0,elt.1,elt2));
				}
			}
		}
		fn size_hint(&self) -> (usize, Option<usize>) {
			let (_, upper) = self.iter.size_hint();
			(0, upper)
		}
	}
}

pub mod cargo_metadata {
	use cargo_metadata;
	use std::path::{PathBuf};

	// https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/util/machine_message.rs
	#[derive(Deserialize,Debug)]
	#[serde(tag = "reason", rename_all = "kebab-case")]
	pub enum Message {
		CompilerArtifact{#[serde(flatten)] artifact: Artifact },
		CompilerMessage{},
		BuildScriptExecuted{},
		#[serde(skip)]
		Unknown, // TODO https://github.com/serde-rs/serde/issues/912
	}
	#[derive(Deserialize,Debug)]
	pub struct Artifact {
		pub package_id: String,
		pub target: cargo_metadata::Target, // https://github.com/rust-lang/cargo/blob/c24a09772c2c1cb315970dbc721f2a42d4515f21/src/cargo/core/manifest.rs#L188
		pub profile: ArtifactProfile,
		pub features: Vec<String>,
		pub filenames: Vec<PathBuf>,
		pub fresh: bool,
	}
	#[derive(Deserialize,Debug)]
	pub struct ArtifactProfile {
		pub opt_level: String,
		pub debuginfo: Option<u32>,
		pub debug_assertions: bool,
		pub overflow_checks: bool,
		pub test: bool,
	}
}

pub mod serde_multiset {
	use std::{fmt,marker,hash};
	use serde::{de,ser};
	use serde::ser::SerializeSeq;
	use multiset;

	pub fn serialize<T: PartialEq+Eq+hash::Hash+ser::Serialize, S: ser::Serializer>(self_: &multiset::HashMultiSet<T>, serializer: S) -> Result<S::Ok, S::Error> {
		let mut seq = serializer.serialize_seq(Some(self_.len()))?;
		for e in self_.iter() {
			seq.serialize_element(e)?;
		}
		seq.end()
	}
	pub fn deserialize<'de, T: PartialEq+Eq+hash::Hash+de::Deserialize<'de>, D: de::Deserializer<'de>>(deserializer: D) -> Result<multiset::HashMultiSet<T>, D::Error> {
		struct Visitor<'de, T: PartialEq+Eq+hash::Hash+de::Deserialize<'de>>(marker::PhantomData<(&'de (),fn()->T)>);
		impl<'de, T: PartialEq+Eq+hash::Hash+de::Deserialize<'de>> de::Visitor<'de> for Visitor<'de,T> {
			type Value = multiset::HashMultiSet<T>;
			fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
				formatter.write_str("an array of elements")
			}
			fn visit_seq<S: de::SeqAccess<'de>>(self, mut seq: S) -> Result<multiset::HashMultiSet<T>, S::Error> {
				let mut x = multiset::HashMultiSet::new();
				while let Some(value) = seq.next_element()? {
					x.insert(value);
				}
				Ok(x)
			}
		}
		deserializer.deserialize_seq(Visitor(marker::PhantomData))
	}
}

pub mod binary_string {
	use std::{fmt,char};
	use std::fmt::Write;
	use serde::{Serializer, de, Deserialize, Deserializer};
	#[derive(PartialEq,Eq,Hash,Serialize,Debug)]
	struct BinaryString(#[serde(with = "self")] Vec<u8>);
	pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
		serializer.collect_str(&Abc(bytes))
	}
	pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error> where D: Deserializer<'de> {
		let s = <String>::deserialize(deserializer)?;
		Ok(s.chars().map(|x:char|{let x = x as u32; assert!(x < 256); x as u8}).collect())
	}
	struct Abc<'a>(&'a [u8]);
	impl<'a> fmt::Display for Abc<'a> {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			for &x in self.0 {
				f.write_char(char::from_u32(x as u32).unwrap()).unwrap();
			}
			Ok(())
		}
	}
}

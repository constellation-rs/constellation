use serde::{de, de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use std::{
	convert::TryInto, fmt::{self, Display}, ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Sub, SubAssign}, str::FromStr
};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Mem(pub(super) u64);
impl Mem {
	pub const B: Mem = Mem(1);
	pub const KIB: Mem = Mem(1024);
	pub const MIB: Mem = Mem(1024 * 1024);
	pub const GIB: Mem = Mem(1024 * 1024 * 1024);
	pub const TIB: Mem = Mem(1024 * 1024 * 1024 * 1024);
	pub const PIB: Mem = Mem(1024 * 1024 * 1024 * 1024 * 1024);
	pub const EIB: Mem = Mem(1024 * 1024 * 1024 * 1024 * 1024 * 1024);
}
impl FromStr for Mem {
	type Err = ();

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut index = 0;
		if index == s.len() {
			return Err(());
		}
		index = s
			.chars()
			.position(|c| !c.is_ascii_digit())
			.unwrap_or(s.len());
		let a: u64 = s[..index].parse().unwrap();
		if index == s.len() {
			return Ok(Mem(a));
		}
		let (b, b1): (u64, u32) = if s[index..=index].chars().next().ok_or(())? == '.' {
			index += 1;
			let index1 = index;
			index = index
				+ s[index..]
					.chars()
					.position(|c| !c.is_ascii_digit())
					.unwrap_or(s.len() - index);
			if index != index1 {
				(
					s[index1..index].parse().unwrap(),
					(index - index1).try_into().unwrap(),
				)
			} else {
				(0, 0)
			}
		} else {
			(0, 0)
		};
		if index + 1 < s.len() && s[index..=index].chars().next().ok_or(())? == ' ' {
			index += 1;
		}
		let c: u64 = match &s[index..] {
			"" | "B" => 1,
			"KiB" => 1024,
			"MiB" => 1024_u64.pow(2),
			"GiB" => 1024_u64.pow(3),
			"TiB" => 1024_u64.pow(4),
			"PiB" => 1024_u64.pow(5),
			"EiB" => 1024_u64.pow(6),
			_ => return Err(()),
		};
		Ok(Mem(a * c + b * c / 10_u64.pow(b1)))
	}
}
impl Display for Mem {
	fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
		write!(f, "{}B", self.0) // TODO
	}
}
impl Add for Mem {
	type Output = Self;

	fn add(self, rhs: Self) -> Self {
		Mem(self
			.0
			.checked_add(rhs.0)
			.expect("overflow when adding mems"))
	}
}
impl AddAssign for Mem {
	fn add_assign(&mut self, rhs: Self) {
		*self = *self + rhs;
	}
}
impl Sub for Mem {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		Mem(self
			.0
			.checked_sub(rhs.0)
			.expect("overflow when subtracting mems"))
	}
}
impl SubAssign for Mem {
	fn sub_assign(&mut self, rhs: Self) {
		*self = *self - rhs;
	}
}
impl Mul<u64> for Mem {
	type Output = Self;

	fn mul(self, rhs: u64) -> Self {
		Mem(self
			.0
			.checked_mul(rhs)
			.expect("overflow when multiplying mem by scalar"))
	}
}
impl Mul<Mem> for u64 {
	type Output = Mem;

	fn mul(self, rhs: Mem) -> Mem {
		rhs * self
	}
}
impl MulAssign<u64> for Mem {
	fn mul_assign(&mut self, rhs: u64) {
		*self = *self * rhs;
	}
}
impl Div<u64> for Mem {
	type Output = Self;

	fn div(self, rhs: u64) -> Self {
		Mem(self
			.0
			.checked_div(rhs)
			.expect("divide by zero error when dividing mem by scalar"))
	}
}
impl DivAssign<u64> for Mem {
	fn div_assign(&mut self, rhs: u64) {
		*self = *self / rhs;
	}
}

impl Serialize for Mem {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		if serializer.is_human_readable() {
			self.to_string().serialize(serializer)
		} else {
			self.0.serialize(serializer)
		}
	}
}
impl<'de> Deserialize<'de> for Mem {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		if deserializer.is_human_readable() {
			deserializer.deserialize_str(MemVisitor)
		} else {
			u64::deserialize(deserializer).map(Mem)
		}
	}
}
struct MemVisitor;
impl<'de> Visitor<'de> for MemVisitor {
	type Value = Mem;

	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
		formatter.write_str("a memory size, like \"800 MiB\" or \"6 GiB\"")
	}

	fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
	where
		E: de::Error,
	{
		Mem::from_str(value)
			.map_err(|()| E::custom(format!("couldn't parse memory size: {}", value)))
	}

	fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
	where
		E: de::Error,
	{
		Ok(Mem(value))
	}
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Cpu(pub(super) u32);
impl Cpu {
	pub const CORE: Cpu = Cpu(65536);
}
impl FromStr for Cpu {
	type Err = ();

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut index = 0;
		if index == s.len() {
			return Err(());
		}
		index = s
			.chars()
			.position(|c| !c.is_ascii_digit())
			.unwrap_or(s.len());
		let a: u32 = s[..index].parse().unwrap();
		if index == s.len() {
			return Ok(Cpu(a * 65536));
		}
		let (b, b1): (u64, u32) = if s[index..=index].chars().next().ok_or(())? == '.' {
			index += 1;
			let index1 = index;
			index = index
				+ s[index..]
					.chars()
					.position(|c| !c.is_ascii_digit())
					.unwrap_or(s.len() - index);
			if index != index1 {
				(
					s[index1..index].parse().unwrap(),
					(index - index1).try_into().unwrap(),
				)
			} else {
				(0, 0)
			}
		} else {
			(0, 0)
		};
		if index < s.len() {
			return Err(());
		}
		let c = 65536;
		Ok(Cpu((u64::from(a) * c + b * c / 10_u64.pow(b1))
			.try_into()
			.unwrap()))
	}
}
impl Display for Cpu {
	fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
		write!(f, "{}", f64::from(self.0) / 65536.0) // TODO
	}
}
impl Add for Cpu {
	type Output = Self;

	fn add(self, rhs: Self) -> Self {
		Cpu(self
			.0
			.checked_add(rhs.0)
			.expect("overflow when adding cpus"))
	}
}
impl AddAssign for Cpu {
	fn add_assign(&mut self, rhs: Self) {
		*self = *self + rhs;
	}
}
impl Sub for Cpu {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		Cpu(self
			.0
			.checked_sub(rhs.0)
			.expect("overflow when subtracting cpus"))
	}
}
impl SubAssign for Cpu {
	fn sub_assign(&mut self, rhs: Self) {
		*self = *self - rhs;
	}
}
impl Mul<u32> for Cpu {
	type Output = Self;

	fn mul(self, rhs: u32) -> Self {
		Cpu(self
			.0
			.checked_mul(rhs)
			.expect("overflow when multiplying cpu by scalar"))
	}
}
impl Mul<Cpu> for u32 {
	type Output = Cpu;

	fn mul(self, rhs: Cpu) -> Cpu {
		rhs * self
	}
}
impl MulAssign<u32> for Cpu {
	fn mul_assign(&mut self, rhs: u32) {
		*self = *self * rhs;
	}
}
impl Div<u32> for Cpu {
	type Output = Self;

	fn div(self, rhs: u32) -> Self {
		Cpu(self
			.0
			.checked_div(rhs)
			.expect("divide by zero error when dividing cpu by scalar"))
	}
}
impl DivAssign<u32> for Cpu {
	fn div_assign(&mut self, rhs: u32) {
		*self = *self / rhs;
	}
}

impl Serialize for Cpu {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		if serializer.is_human_readable() {
			self.to_string().serialize(serializer)
		} else {
			self.0.serialize(serializer)
		}
	}
}
impl<'de> Deserialize<'de> for Cpu {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		if deserializer.is_human_readable() {
			deserializer.deserialize_any(CpuVisitor)
		} else {
			u32::deserialize(deserializer).map(Cpu)
		}
	}
}
struct CpuVisitor;
impl<'de> Visitor<'de> for CpuVisitor {
	type Value = Cpu;

	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
		formatter.write_str("a quantity of logical CPU cores, like \"0.5\" or \"4\"")
	}

	fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
	where
		E: de::Error,
	{
		Cpu::from_str(value)
			.map_err(|()| E::custom(format!("couldn't parse CPU quantity: {}", value)))
	}

	fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
	where
		E: de::Error,
	{
		Ok(Cpu((value * 65536).try_into().map_err(|_| {
			E::custom(format!("couldn't parse CPU quantity: {}", value))
		})?))
	}
	fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
	where
		E: de::Error,
	{
		self.visit_u64(
			value
				.try_into()
				.map_err(|_| E::custom(format!("couldn't parse CPU quantity: {}", value)))?,
		)
	}
	#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
	fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
	where
		E: de::Error,
	{
		Ok(Cpu((value * 65536.0) as u32)) // TODO
	}
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
	use std::str::FromStr;

	use super::{Cpu, Mem};

	#[test]
	fn parse_mem_size_test() {
		assert_eq!(Mem::from_str(""), Err(()));
		assert_eq!(
			Mem::from_str("1.500000001GiB"),
			Ok(Mem(1024_u64.pow(3) * 3 / 2 + 1))
		);
		assert_eq!(
			Mem::from_str("1.9999999999GiB"),
			Ok(Mem(2 * 1024_u64.pow(3) - 1))
		);
		assert_eq!(
			Mem::from_str("1.999999999GiB"),
			Ok(Mem(2 * 1024_u64.pow(3) - 2))
		);
		assert_eq!(Mem::from_str("1.000000000GiB"), Ok(Mem(1024_u64.pow(3))));
		assert_eq!(Mem::from_str("1.0000000001GiB"), Ok(Mem(1024_u64.pow(3))));
		assert_eq!(
			Mem::from_str("1.000000001GiB"),
			Ok(Mem(1024_u64.pow(3) + 1))
		);
		for i in 0..100_000 {
			assert_eq!(Mem::from_str(&i.to_string()), Ok(Mem(i)));
		}
		for i in 0..100_000 {
			assert_eq!(Mem::from_str(&format!("{}B", i)), Ok(Mem(i)));
		}
		for i in 0..100_000 {
			assert_eq!(
				Mem::from_str(&format!("{}TiB", i)),
				Ok(Mem(i * 1024_u64.pow(4)))
			);
		}
		for i in 0..1000 {
			assert_eq!(
				Mem::from_str(&format!("1.{:03}KiB", i)),
				Ok(Mem(1024_u64.pow(1) + i * 1024_u64.pow(1) / 1000))
			);
		}
		for i in 0..1000 {
			assert_eq!(
				Mem::from_str(&format!("1.{:03}GiB", i)),
				Ok(Mem(1024_u64.pow(3) + i * 1024_u64.pow(3) / 1000))
			);
		}
	}

	#[test]
	fn parse_cpu_size_test() {
		assert_eq!(Cpu::from_str("123"), Ok(Cpu(123 * 65536)));
		assert_eq!(Cpu::from_str("123.5"), Ok(Cpu(247 * 65536 / 2)));
		assert_eq!(Cpu::from_str("123.25"), Ok(Cpu(493 * 65536 / 4)));
		assert_eq!(Cpu::from_str("123.25"), Ok(Cpu(493 * 65536 / 4)));
		assert_eq!(Cpu::from_str("0.0000152587890625"), Ok(Cpu(1)));
		assert_eq!(Cpu::from_str("0.0000152587890624"), Ok(Cpu(0)));
		assert_eq!(Cpu::from_str("0.0000152587890626"), Ok(Cpu(1)));
		assert_eq!(
			Cpu::from_str("65535.0000152587890625"),
			Ok(Cpu(65535 * 65536 + 1))
		);
		assert_eq!(
			Cpu::from_str("65535.0000152587890624"),
			Ok(Cpu(65535 * 65536))
		);
		assert_eq!(
			Cpu::from_str("65535.0000152587890626"),
			Ok(Cpu(65535 * 65536 + 1))
		);
		// assert_eq!(Cpu::from_str("0.9999847412109374"), Ok(Cpu(65535)));
		// assert_eq!(Cpu::from_str("65535.9999847412109374"), Ok(Cpu(u32::max_value())));
	}
}

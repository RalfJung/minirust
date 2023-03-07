The endianness defines how integers and pointers are encoded and decoded.
This is defined by specr, we just repeat the signature here for reference.

```rust,ignore
pub enum Endianness {
    LittleEndian,
    BigEndian,
}

pub use Endianness::*;

impl Endianness {
    /// If `signed == Signed`, the data is interpreted as two's complement.
    pub fn decode(self, signed: Signedness, bytes: List<u8>) -> Int;

    /// This can fail (return `None`) if the `int` does not fit into `size` bytes,
    /// or if it is negative and `signed == Unsigned`.
    pub fn encode(self, signed: Signedness, size: Size, int: Int) -> Option<List<u8>>;
}
```

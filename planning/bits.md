# bits(N)

Status: type landed (same machinery slice as sint); the distinguishing
features land with their owning workstreams (below).

`bits(N)` is a raw N-bit vector. It deliberately shares uint's width
machinery — const-arg widths, literal inference and fit checks, generic
matching, `logic [N-1:0]` emission — while being a different KIND of thing:

- **uint/sint are numbers; bits is a bag of bits.** bits implements `Eq`
  (pattern equality) and nothing else: `+`, `*`, `<`, unary `-` on bits are
  type errors. No implicit conversion in either direction — a uint is not
  its representation.
- **Default printing is hex.** A decimal-written literal that types as
  bits emits as sized hex (`bits(8)` from `200` → `8'hC8`); written hex
  and binary keep their base, as everywhere.

The reasons it exists, each owned by later work:

1. **pack/unpack target.** The `Bits` trait (planning/traits.md T4
   customer) packs to and unpacks from `bits(Self::width)` — NOT uint:
   a packed struct is a representation, and arithmetic on it is almost
   always a bug.
2. **Slicing.** `x[8..4]` / `x[3]` belong to bits directly (planning/slicing.md
   — half-open, bits written high-first); on uint/sint, slicing goes through
   bits (`u.pack()[..]`), keeping "number" and "bit field" honest.
3. **X at the BIT level.** When Mirin grows 4-state semantics, `bits` is
   where per-bit X lives. uint/sint get VALUE-level semantics (a uint
   with any X bit is a poisoned value, not a partially-known number) —
   this is where Mirin diverges from Verilog's logic-everywhere, and bits
   is the honest carrier for the Verilog-shaped case.

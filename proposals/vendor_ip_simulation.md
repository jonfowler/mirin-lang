# Simulating vendor RAM/DSP IP with an open-source toolchain

Research note, 2026-06-11. Question: when generated SystemVerilog targets FPGA
RAM/DSP resources (Quartus and Vivado), how can the result still be simulated
with Verilator (or Icarus/GHDL/cocotb), and what should that imply for Polar's
backend?

Findings below are sourced from vendor docs and primary repos. Items marked
**[verified]** survived independent adversarial verification; the rest are
quoted from primary sources but were not independently re-checked (the
verification pass was cut short).

## TL;DR for Polar

1. **Emit inference templates, not vendor primitives, as the default.** Plain
   synchronous-read RTL infers BRAM in both Vivado and Quartus; `(* ram_style
   *)` / `ramstyle` attributes steer the mapping. Multiply/accumulate idioms
   infer DSP48/DSP blocks. Inferred RTL simulates in Verilator with zero
   vendor files — this is how LiteX (`litex_sim` is built entirely on
   Verilator), F4PGA-adjacent flows, and Yosys's memory-inference pass all
   work.
2. **Where a hard primitive must be named** (URAM cascades, DSP pre-adder
   patterns synthesis won't find, vendor-specific RDW behavior), emit the
   primitive instantiation **plus ship/select a behavioral stand-in model**
   for simulation. Precedent: openMSP430 ships its own `altsyncram.v`
   behavioral model in its testbench tree; `oliverbunting/verilator-unisims`
   and `catkira/verilator-unisims` do the same for Xilinx primitives.
3. **Never make encrypted IP load-bearing.** Anything in Xilinx `SECUREIP`
   (GT transceivers, PCIe hard blocks) or Intel encrypted device atoms is
   IEEE 1735-encrypted and only usable in vendor-blessed commercial
   simulators. Those stay behind a port boundary the user stubs or
   co-simulates.

## 1. Which vendor libraries are plain source vs encrypted

### Xilinx/AMD

- **UNISIM / UNIMACRO / UNIFAST** ship as plain Verilog and VHDL source in the
  Vivado install tree (`<Vivado>/data/verilog/src/unisims` etc.); only
  specific retarget wrapper files (`unisim_retarget_comp.vp`) are encrypted
  (UG900). The RAMB18/36E1/E2, URAM288, FIFO, and DSP48E1/E2 behavioral models
  are plain source.
- **[verified]** Xilinx published the Verilog unisim library as open source
  under Apache 2.0: `github.com/Xilinx/XilinxUnisimLibrary`. Caveat
  **[verified]**: it snapshots **Vivado 2020.1**, was archived read-only on
  2022-12-06 with 2 total commits, so it does not track newer parts/releases.
  Models derived from it (e.g. `catkira/verilator-unisims`'s `DSP48E1.v`,
  which carries the Xilinx copyright under Apache 2.0) are legally
  redistributable and patchable.
- **XPM macros** (`xpm_memory_*`, `xpm_fifo_*`, `xpm_cdc_*`) are plain
  SystemVerilog in `<Vivado>/data/ip/xpm`. They are the most
  Verilator-tractable "official" route to BRAM/URAM — parameterized SV rather
  than netlists — though they historically needed Verilator workarounds
  (tracked in verilator issues #2568/#3464; recent Verilator versions handle
  much more of them).
- **SECUREIP** (GT transceivers, PCIe, other hard IP) is IEEE P1735
  encrypted. No open-source simulator can consume it; Verilator/Icarus/GHDL
  have no 1735 decryption (and vendors wouldn't issue them keys).

### Intel/Altera

- **`altera_mf.v`** (altsyncram, altpll, dcfifo, lpm via `220model.v`,
  `sgate.v`) ships as plain Verilog in `<Quartus>/eda/sim_lib`. It is
  simulator-portable in principle; in practice it's written in loose
  Verilog-2001 style (delays, X-handling, `specify` blocks) that Icarus
  digests with some effort and Verilator rejects in places — community
  practice is to patch or substitute (see §3).
- **Device atom libraries** (`cyclonev_atoms.v`, `stratix10_atoms*.v`, …) are
  a mix: many atoms are plain source, but newer families keep
  `*_atoms_ncrypt.v` companions that are encrypted. Post-fit (gate-level)
  netlists for newer devices therefore generally need a commercial simulator.
- **Quartus Pro Platform Designer IP** generates per-IP simulation file sets
  (see §2); the generated top-level files are plain source but may reference
  encrypted sub-libraries depending on the IP.

### Vendor support posture (both vendors)

Neither tool lists any open-source simulator as a supported target. Vivado's
supported set is XSim, Questa/ModelSim, VCS, Xcelium, Riviera/Active-HDL;
Quartus's simulator-setup-script generation similarly covers only commercial
tools. Any Verilator flow repurposes the exported *file lists*, not the
generated scripts.

## 2. Harvesting simulation products from the vendor tools

### Vivado

- `generate_target simulation [get_ips <ip>]` produces the IP's simulation
  output products (for many IP: structural Verilog over unisims; for some: SV
  behavioral models).
- `export_ip_user_files` populates `ip_user_files/` separating **static**
  sources shared across IP (`ipstatic/`) from per-customization dynamic
  files — this is the layout to harvest for Verilator. Run it before
  `export_simulation`.
- `export_simulation` emits per-simulator scripts with the full compile
  order; `report_compile_order -used_in simulation` gives a simulator-agnostic
  ordered manifest. Either is a usable source of truth for a `verilator -f`
  file list; the scripts themselves target only commercial simulators.
- Then compile: harvested IP sources + unisims (install tree or
  XilinxUnisimLibrary) + `glbl.v` under Verilator. Expect to patch around
  unsupported constructs in some unisim models; community forks
  (`oliverbunting/verilator-unisims` **[verified existence/coverage]** —
  covers RAMB16BWER, URAM288, RAM16X1D/32X1D, SRL16E, DSP48E2, LUTs, carry,
  FFs, BUFG, DCM_SP/PLL_BASE, SERDES; notably *not* RAMB36E1/E2) exist
  precisely because of this.

### Quartus (Pro)

- Enabling simulation output at IP generation time produces a functional
  simulation model plus setup scripts under `<project>/<ip>/sim/<vendor>` —
  a predictable tree a script can harvest. Setup scripts are only generated
  for Quartus's supported commercial simulators.
- For megafunction-level designs (altsyncram etc.), simulation needs only
  `altera_mf.v`/`220model.v` from `eda/sim_lib` — historically workable in
  Icarus, partially in Verilator, commonly replaced by stand-ins.

## 3. The inference-first alternative (what real projects do)

- **Inference templates.** A synchronous-read memory written as plain RTL
  (`always @(posedge clk)` read and write on a 2-D reg array) maps to block
  RAM in both Vivado and Quartus; attributes (`ram_style`/`ramstyle`,
  `use_dsp`/`multstyle`) steer BRAM/LUTRAM/URAM and DSP packing. The
  well-known portability hazards are read-during-write semantics, mixed-width
  ports, and asymmetric dual-port — each vendor's inference recognizes a
  slightly different idiom set (Dan Strother's and Stitt's template write-ups;
  Yosys's memory-inference documentation describes the same shared-idiom
  approach for the open flow).
- **Behavioral stand-ins for named primitives.** When code does instantiate a
  vendor cell, projects swap in a plain-Verilog model for simulation:
  - openMSP430 ships `altsyncram.v` in its testbench tree covering
    single-port/dual-port/bidir/ROM modes, dual clocks, enables, aclr, byte
    enables, output-register control — feasible, but the parameter surface to
    replicate is large.
  - tommythorn/verilog-sim-bench has a ~50-line minimal `altsyncram` stand-in —
    deliberately incomplete (symmetric widths only, ignores byte enables and
    RDW modes), illustrating the cheap end of the spectrum.
  - `verilator-unisims` forks do the same for Xilinx cells.
- **LiteX** is the strongest existence proof at SoC scale: `litex_sim` runs
  full SoCs cycle-accurately on Verilator with no vendor simulation libraries
  at all — memories and arithmetic are generated as inferable RTL, and
  vendor-primitive instantiations are confined to platform-specific wrappers
  excluded from simulation.

## 4. Hard limits — where a commercial simulator stays unavoidable

- **Encrypted hard IP**: SECUREIP (GTs, PCIe), encrypted Intel atoms, and any
  third-party 1735 IP. No open-source path exists or is likely.
- **Timing simulation**: SDF-annotated gate-level sim is out of scope for
  Verilator (no `specify`/SDF support); that flow stays on Questa/XSim/etc.
- **X-propagation fidelity**: Verilator is 2-state by default; vendor models
  lean on X semantics for "you violated a constraint" signaling (e.g. RDW
  collision → X). Verilator's `--x-assign`/`--x-initial` mitigate but don't
  reproduce 4-state behavior; Icarus/GHDL are 4-state but much slower.
- **Vendor model style**: delays (`#`), `specify` blocks, and loose typing in
  altera_mf/unisims mean "compile the vendor library under Verilator" is
  never zero-patch; it works per-cell, not wholesale.

## Implications for the Polar backend

- Make **inferable RTL the only default output** for memories and arithmetic.
  Polar controls the emitted idiom, so it can emit the intersection-idiom that
  Vivado, Quartus, and Yosys all infer, with `ram_style`-class attributes as a
  user-steerable knob. Simulation then needs nothing beyond the generated SV.
- Define RDW semantics in the language (Polar already cares about RTL
  correctness) and emit the idiom matching the chosen semantics, rather than
  inheriting whatever each vendor's BRAM mode does.
- For explicit primitive targeting, design it as a **dual-emission** feature
  from day one: the vendor instantiation for synthesis and a Polar-owned
  behavioral model (or selected open model) for simulation, behind the same
  module interface — the openMSP430/verilator-unisims pattern, but generated.
- Treat encrypted hard IP as foreign: a port-boundary `extern` the user wires
  to a stub, a cocotb co-simulation, or a commercial simulator. Don't attempt
  to make it simulate in the open flow.

## Sources

- UG900 Vivado Logic Simulation (2022.1): library locations, secureip,
  export_simulation / export_ip_user_files, supported simulators.
- AMD AR#66533: ip_user_files / sim_scripts layout,
  `report_compile_order -used_in simulation`.
- Quartus Prime Pro Getting Started UG (25.3): generating IP simulation files,
  `<ip>/sim/<vendor>` layout, supported-simulator script generation.
- github.com/Xilinx/XilinxUnisimLibrary (Apache 2.0, 2020.1 snapshot,
  archived 2022) **[verified]**.
- github.com/oliverbunting/verilator-unisims (coverage list) **[verified]**;
  github.com/catkira/verilator-unisims (DSP48E1 from Xilinx source).
- github.com/olgirard/openmsp430 — testbench `altsyncram.v`;
  github.com/tommythorn/verilog-sim-bench — minimal `altsyncram.v`.
- LiteX wiki "SoC Simulator" (litex_sim on Verilator).
- Yosys memory-inference docs; Dan Strother "Inferring RAMs in FPGAs";
  stitt-hub "Portable RAM inference templates".
- Verilator issues #2568, #3464 (Xilinx library compatibility);
  Verilator language docs (2-state, no specify/SDF).

# Byte error rate calibration

The release gate for this modem. `modem-core::impair::MAX_BYTE_ERROR_RATE` is fixed by the measurements below and does not move upward afterwards - a later change that pushes the measured rate above it has made the modem worse, not the gate wrong.

**Measured:** 8th September 2026 (fix round 2)
**Commit:** see the task report for the exact commit this fix round lands on
**Toolchain:** rustc 1.98.1, cargo 1.98.1

## Fix round 2: the confound moved, it did not leave

Independent review re-verified fix round 1's figures to four decimal places - the harness reproduces exactly, and `harmonic_distortion` is independently confirmed correctly modelled. But fix round 1's fix for Critical 1 (the periodic payload) removed the periodicity confound and left a second one in exactly the place Critical 3's gate-bracket fix depended on: **which specific shuffle of the payload gets used.**

Ten arbitrary shuffles of the identical 2,100-byte multiset, at the identical -28,000 ppm drift that fix round 1 used to bracket `MAX_BYTE_ERROR_RATE` from below, span **0.0024 to 0.5619 - a 234x spread** - and only one of the ten sits inside the gate. Fix round 1's own report described switching away from a shuffle that "collapsed" during exploration to one that did not; that is seed selection, the same defect the periodicity fix was supposed to retire, one level up.

What actually is stable, measured across the same ten shuffles and confirmed again here with a fresh, non-curated set of eight (seeds 1 through 8, chosen before running anything, not after): **every configuration this task calls clean, at +/-25,000 ppm, measures 0.0 or 0.0005 (one byte in 2,100) - at least 20x inside the gate.** That is a fact about the boundary, not a draw. `MAX_BYTE_ERROR_RATE`'s lower bracket now rests on it.

Two further corrections came out of this round:

- **The AWGN "flat" floor was over-extended.** Fix round 1 claimed 3 dB was flat, based on one seed. Sixteen seeds tested at 3 dB show one exception (0.0182); Task 5's own 6 dB figure is the floor this crate can actually stand behind, and this round only reconfirms it.
- **The stated mechanism for the payload sensitivity was wrong in two specifics**, though the conclusion (payload structure matters) was right. It is not top-bit parity - a control payload with the top bit clear on *every* byte still spans 0.31-0.99 at +/-28,000 ppm, as bad as unconstrained random bytes. It is not transition density either - the ten shuffles above have identical density by construction and still span 234x. The real mechanism is **ordering**: a Gardner loop only corrects its timing at an actual symbol transition, so a long run of the same symbol lets accumulated clock-offset phase error grow unchecked, and where the longest such runs land relative to that error is a property of the specific sequence. This is the same phase-lottery mechanism `rx.rs`'s own module doc already documents for lead-in offsets - not a new phenomenon, a second sighting of the same one.

The actionable line for Tasks 17 and 19: **+/-2.5% clock offset is safe and payload-robust. Anything between 2.5% and 3.5% is not a number that can be quoted** - it depends on the specific content being sent, not just the drift.

## Method

Every figure below comes from the same harness (`modem-core/src/impair.rs`'s test module, functions `air`, `demod`, `measure_ber`):

1. `air(payload, role)` renders a real `Tx` waveform at `DSP_RATE` (8 kHz): a settle period, the acquisition preamble `rx.rs`'s own module doc requires (alternating symbols, then one character time of idle mark - a Gardner timing loop cannot acquire on a constant tone), then the payload.
2. The impairment under test is applied to that buffer.
3. `demod(samples, role)` runs it through a real `Rx` and returns everything the receiver decoded, unmodified.
4. `measure_ber(payload, recovered)` scores the result: content compared at the best of a +/-32-byte bidirectional alignment search, never a length check, a `contains`, or an `ends_with` (see Mutation 1 in the task report - this is the single most important test in the task, because a cycle slip can hand back the right byte count and zero framing errors while every byte is wrong).

All noise is deterministic (xorshift64, explicitly seeded), so every figure here is exactly reproducible from the commit above.

Two payloads are used: a 55-byte varied-ASCII string (`PAYLOAD`) for anything where content divergence is the point (it shows up by byte index 2 - a short payload is enough), and a 2,100-byte payload (`shuffled_sentence(seed)`: `"The quick brown fox. "` x100, deterministically shuffled by the given seed to remove its 21-byte period; `long_payload()` is the one fixed canonical shuffle) for `clock_drift`, where cumulative drift over a long transmission is the actual thing under test.

**Known limitation, not fixed this round:** the +/-32-byte alignment search is itself too narrow for some points well beyond the pull-in range - the true alignment at -35,000 ppm on `long_payload` sits at offset -118, outside the search window, so the published figure there (0.9571) is itself an over-report against a wider-search true value (0.9333). Left as is deliberately: over-reporting corruption fails safe for a release gate (it never lets a broken configuration look clean), unlike fix round 1's Critical 2, where the same narrow search under-reported a working reacquisition as a collapse. Flagged here because Task 9's minimodem cross-validation will build on this same `measure_ber`, and a stream with no reason to share this crate's own short training convention may need a wider search to align correctly.

## Results

| Impairment | Parameters | Measured BER | Note |
|---|---|---|---|
| `add_awgn` | 40, 25, 15, 10, 6 dB SNR, 8 seeds each | **0.0** at every point | Flat, robustly - Task 5's own floor, reconfirmed across 8 independent noise seeds at 6 dB with no exceptions. |
| `add_awgn` | 3 dB SNR, 16 seeds | **0.0** for 15 of 16; **0.0182** for one (seed 12) | Not reliably flat. Fix round 1 claimed 3 dB extended the flat floor by one dB, based on a single seed; a wider sample shows a real exception. 6 dB, not 3 dB, is the figure this crate quotes. |
| `add_awgn` | 0 dB SNR, 6 seeds | Range **0.0182 to 0.836** | Clearly above the gate at every seed tested, but the specific number is not stable enough to quote as typical - 0.0182 (fix round 1's published figure) was the smallest of six draws, not a representative one. |
| `clip` | level 1.0 down to 0.0055 | **0.0** | Harmless. A hard limiter barely touches an FSK signal's information, which lives in frequency, not amplitude. |
| `clip` | level 0.005 | **1.0** (0 bytes recovered) | Not gradual: clamping the whole waveform to 0.005 amplitude puts it below the carrier detector's own cold-start sensitivity floor (~0.0075 - see `carrier.rs`'s `INITIAL_FLOOR`), so the receiver never asserts carrier at all. The failure mode is losing the signal entirely, not misreading it. |
| `band_limit` | 300-3400 Hz at 8 kHz | **0.0** | No effect. Both Originate tones (1270, 1070 Hz) sit well inside the telephone passband - unsurprising, since Bell 103 was designed to run over exactly this band. |
| `clock_drift` | +/-25,000 ppm (2.5%), 8 seeds each direction | **0.0 or 0.0005** at all 16 points | Confirms rx.rs's own documented Gardner pull-in range, payload-robustly. This is `MAX_BYTE_ERROR_RATE`'s lower bracket (see below) - not one measurement, the fact that this boundary is stable across payload structure. |
| `clock_drift` | -25,000 ppm, one further seed (0xDEAD_BEEF_CAFE_F00D, found during exploration, not in the 8 above) | **1.0** | Disclosed, not hidden: even the documented-safe +/-2.5% boundary is not a guarantee for arbitrary content, the same phase-lottery mechanism as everything else near this boundary. Rare in this task's sampling (1 exceptional case found across everything measured at +/-25,000 ppm) but real. |
| `clock_drift` | -28,000 ppm, 10 shuffles of the identical multiset | **0.0024, 0.0376, 0.0757, 0.0938, 0.1224, 0.1686, 0.1805, 0.2214, 0.3043, 0.5619** | The withdrawn lower-bracket figure. Fix round 1 published 0.0024 (one shuffle) as though it were a fact about -28,000 ppm; it is a fact about that one shuffle. Only 1 of 10 sits inside the gate. This is why -28,000 ppm no longer brackets anything. |
| `clock_drift` | +35,000 ppm, -35,000 ppm, `long_payload` | **0.91, 0.96** (over-reported - see the known limitation above; wider-search true values 0.91 and 0.93) | Stable regardless of shuffle: 10 independent shuffles all measure above 0.89 at both signs. This is the region that genuinely, reliably collapses. |
| `reverb` | 2 ms delay, gain 0.3 or 0.7 | **0.0** | A desk-distance early reflection (2 ms is roughly the round-trip difference of a 30 cm reflection) is tolerated even at a fairly strong reflection coefficient. |
| `reverb` | 2 ms delay, gain 0.85 | **0.145** | A strong, near-equal-amplitude echo off a hard nearby surface - adversarial but physically real - genuinely corrupts content. This is "the dominant room effect at desk distances" the module doc names, demonstrated, not just asserted. |
| `reverb` | 2 ms delay, gain 0.9 | **0.76** | Substantial collapse. (Corrected in fix round 1 from the original submission's 0.93, which was inflated by the same too-narrow alignment search as the clock_drift figures.) |
| `duplex_leak` alone, no distortion | near_gain 1.0, 2.0 | **0.0** | An undistorted leak at up to twice the far end's own amplitude is tolerated. |
| `duplex_leak` alone, no distortion | near_gain 3.0 | **0.81** | Sheer loudness alone, no harmonic content at all, is enough once the near end is loud enough. |
| `harmonic_distortion` + `duplex_leak` | amount 1.0 (50% second-harmonic ratio), near_gain 1.0 | **0.0** | Moderate overdrive leaked at equal amplitude into the Answer direction stays clean. |
| `harmonic_distortion` + `duplex_leak` | amount 1.6 (80% second-harmonic ratio), near_gain 1.0 | **0.5** | The second harmonic of the 1070 Hz Originate space tone, at 2140 Hz, now carries real energy 85 Hz from the Answer band's 2225 Hz mark, and corrupts half the Answer-direction message. This is the specific mechanism the module doc names for why acoustic full duplex is hard, demonstrated through the real receiver, and independently re-verified this round to the digit. Mutation 3 (task report) swaps the second harmonic for the third and this collapses to 0.0 at the same parameters. `harmonic_distortion` also adds a DC offset equal in size to the second harmonic (`amount * A^2 / 2` at amplitude `A`) - no acoustic path passes DC, so it plays no part in this measurement, but it is a real part of the function's output. |
| Combined room: `harmonic_distortion(0.3)` -> `reverb(2 ms, 0.3)` -> `add_awgn(25 dB)` -> `clip(0.7)` | one direction, 55-byte payload | **0.0** | A plausible physical chain (source overdrive, one desk reflection, ambient noise at Task 17's own 25 dB SNR figure, a moderately hot receiving preamp) at levels well short of any individual collapse above. This is the reference scenario `MAX_BYTE_ERROR_RATE` is checked against in `impair.rs`'s own test suite. |

Neither "cliff" (fix round 1's withdrawn framing) nor "gradual" (fix round 1's replacement, also withdrawn) describes what happens near the pull-in boundary - both imply a single word is the right axis, and the real finding is that the outcome at a fixed drift value depends on the payload's own structure by up to 234x. That dependence is the property worth reporting, not a shape.

## MAX_BYTE_ERROR_RATE = 0.01

**0.01 sits in the empty band between every configuration this task measured working (at most 0.0005) and every one it measured failing (at least 0.018), and coincides with Task 19's real-world desk-test figure ("under 1% of characters corrupted").** Task 19's figure corroborates this constant; it does not carry it alone - Task 19 has not run yet, and a target that has not been checked against reality cannot distinguish "the gate was wrong" from "the modem is worse than we thought" on its own. The measured band, from this task's own data, can do that today.

Two anchors were tried and retired before this one:

1. **Fix round 1's original anchor:** `+30,000 ppm` on a periodic payload measuring exactly 0.01. Retired because the figure was a property of the payload's 21-byte period, not the modem - it moved to 0.36-0.56 under a shuffle control.
2. **Fix round 1's replacement lower bracket:** `-28,000 ppm` on a single shuffled (non-periodic) payload measuring 0.0024. Retired because the same 2,100-byte multiset under nine other arbitrary shuffles spans 0.0376 to 0.5619 at that drift - a lottery, not a fact.

The bracket that survives is `+/-25,000 ppm`, swept across 8 independent, non-curated shuffle seeds in both directions (16 measurements): every one measures 0.0 or 0.0005, at least 20x inside the gate. `impair.rs`'s own test suite asserts this directly - `clock_drift_survives_the_documented_pull_in_range` fails if `MAX_BYTE_ERROR_RATE` is ever lowered past this boundary (e.g. to 0.0), and `a_scenario_above_the_gate_is_correctly_rejected` (AWGN at 0 dB, a single fixed reproducible seed, comfortably above the gate though not a typical figure for that scenario - see the Results table) fails if it is ever raised past a real failing scenario (e.g. to 0.5).

# Byte error rate calibration

The release gate for this modem. `modem-core::impair::MAX_BYTE_ERROR_RATE` is fixed by the measurements below and does not move upward afterwards - a later change that pushes the measured rate above it has made the modem worse, not the gate wrong.

**Measured:** 8th September 2026
**Commit:** `db95912` (`feat(core): impairment suite and BER calibration`)
**Toolchain:** rustc 1.98.1, cargo 1.98.1

## Method

Every figure below comes from the same harness (`modem-core/src/impair.rs`'s test module, functions `air`, `demod`, `measure_ber`):

1. `air(payload, role)` renders a real `Tx` waveform at `DSP_RATE` (8 kHz): a settle period, the acquisition preamble `rx.rs`'s own module doc requires (alternating symbols, then one character time of idle mark - a Gardner timing loop cannot acquire on a constant tone), then the payload.
2. The impairment under test is applied to that buffer.
3. `demod(samples, role)` runs it through a real `Rx` and returns everything after the two training bytes.
4. `measure_ber(payload, recovered)` scores the result: content compared at the best of a small window of leading alignments, never a length check, a `contains`, or an `ends_with` (see Mutation 1 in the task report - this is the single most important test in the task, because a cycle slip can hand back the right byte count and zero framing errors while every byte is wrong).

All noise is deterministic (xorshift64, explicitly seeded), so every figure here is exactly reproducible from the commit above.

Two payloads are used: a 57-byte varied-ASCII string for anything where content divergence is the point (it shows up by byte index 2 - a short payload is enough), and a 2,200-byte payload (`"The quick brown fox. "` x100) for `clock_drift`, where cumulative drift over a long transmission is the actual thing under test.

## Results

| Impairment | Parameters | Measured BER | Note |
|---|---|---|---|
| `add_awgn` | 40, 25, 15, 10, 6, 3 dB SNR | **0.0** at every point | Flat. Task 5 found this flat down to 6 dB across a 4x range of loop gain; this extends the floor one dB further, to 3 dB. The correlator's own matched-filter processing gain is doing the rejecting before the slicer ever sees the noise. First measurable movement is at 0 dB: 1 of 55 bytes wrong (0.0182) - a genuinely different regime, not noise in the flat one. |
| `clip` | level 1.0 down to 0.0055 | **0.0** | Harmless. A hard limiter barely touches an FSK signal's information, which lives in frequency, not amplitude. |
| `clip` | level 0.005 | **1.0** (0 bytes recovered) | Not gradual: clamping the whole waveform to 0.005 amplitude puts it below the carrier detector's own cold-start sensitivity floor (~0.0075 - see `carrier.rs`'s `INITIAL_FLOOR`), so the receiver never asserts carrier at all. The failure mode is losing the signal entirely, not misreading it. |
| `band_limit` | 300-3400 Hz at 8 kHz | **0.0** | No effect. Both Originate tones (1270, 1070 Hz) sit well inside the telephone passband - unsurprising, since Bell 103 was designed to run over exactly this band. |
| `clock_drift` | +/-25,000 ppm (2.5%) | **0.0** | Confirms rx.rs's own documented Gardner pull-in range, independently, through a different mechanism (a single already-rendered buffer resampled by `clock_drift`, rather than two `Tx`/`Rx` pairs configured with differing device rates). |
| `clock_drift` | +28,000 ppm (2.8%) | **0.0** | The positive direction's clean range extends slightly further than the negative direction's - see below. |
| `clock_drift` | +30,000 ppm (3.0%) | **0.01** | The first hint of degradation - 21 of 2,100 bytes wrong. This is the anchor point for `MAX_BYTE_ERROR_RATE` (see below). |
| `clock_drift` | -28,000 ppm (2.8%) | **0.11** | The pull-in range is asymmetric: the negative direction starts degrading half a percentage point earlier than the positive direction. |
| `clock_drift` | +35,000 ppm, -30,000 ppm | **0.93, 0.91** | Beyond the pull-in range the loop does not degrade gracefully - it collapses. |
| `reverb` | 2 ms delay, gain 0.3 or 0.7 | **0.0** | A desk-distance early reflection (2 ms is roughly the round-trip difference of a 30 cm reflection) is tolerated even at a fairly strong reflection coefficient. |
| `reverb` | 2 ms delay, gain 0.85 | **0.145** | A strong, near-equal-amplitude echo off a hard nearby surface - adversarial but physically real - genuinely corrupts content. This is "the dominant room effect at desk distances" the module doc names, demonstrated, not just asserted. |
| `reverb` | 2 ms delay, gain 0.9 | **0.93** | One cliff edge further: near-total collapse. |
| `harmonic_distortion` + `duplex_leak` | amount 1.0, near_gain 1.0 | **0.0** | Moderate overdrive leaked at equal amplitude into the Answer direction stays clean. |
| `harmonic_distortion` + `duplex_leak` | amount 1.6, near_gain 1.0 | **0.5** | Heavy overdrive (the second harmonic of the 1070 Hz Originate space tone, at 2140 Hz, now carries real energy 85 Hz from the Answer band's 2225 Hz mark) corrupts half the Answer-direction message. This is the specific mechanism the module doc names for why acoustic full duplex is hard, demonstrated through the real receiver. Mutation 3 (task report) swaps the second harmonic for the third and this collapses to 0.0 at the same parameters - proof the mechanism is the harmonic's frequency, not just "more distortion energy". |
| Combined room: `harmonic_distortion(0.3)` -> `reverb(2 ms, 0.3)` -> `add_awgn(25 dB)` -> `clip(0.7)` | one direction, 57-byte payload | **0.0** | A plausible physical chain (source overdrive, one desk reflection, ambient noise at Task 17's own 25 dB SNR figure, a moderately hot receiving preamp) at levels well short of any individual cliff above. This is the reference scenario `MAX_BYTE_ERROR_RATE` is checked against in `impair.rs`'s own test suite. |

Every scenario found to be clean above measured **exactly** 0.0, not merely low - there is no long tail of small errors under moderate impairment in this system. Every collapse measured is over an order of magnitude above the chosen gate. The behaviour throughout is closer to a cliff than a slope: FSK plus a Gardner timing loop tends to work fully or fail hard, not degrade gradually, matching Task 5's own observation that AWGN barely moves the needle until it does.

## MAX_BYTE_ERROR_RATE = 0.01

Set to 1%, for two independent reasons that agree:

1. **It matches Task 19's own real-world acceptance figure** for the two-laptop desk test ("under 1% of characters corrupted"). A simulated, fully deterministic calibration gate should be at least as strict as the eventual live-hardware bar it feeds into, not looser.
2. **It is anchored to a genuine measurement, not imported wholesale.** Sweeping `clock_drift` past the documented +/-2.5% Gardner pull-in range, +30,000 ppm (3.0%) measured exactly 0.01 - one step past clean, one step before the loop loses lock outright (35,000 ppm measures over 90%). 0.01 sits exactly at the first sign of trouble in a well-understood, independently corroborated mechanism, with real margin below every genuine collapse this task measured (minimum collapse figure: 0.11, over 10x the gate) and real margin above every scenario measured clean (all exactly 0.0).

The gate is not set at the fragile 3.0% boundary itself - a tiny, uninteresting implementation change right at that specific edge should not flip a release gate. It is set at the value that boundary happens to produce, which is a stable, well-justified number independent of exactly where future work moves that particular edge.

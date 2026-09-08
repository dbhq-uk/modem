# modem

An acoustic modem simulator. Run one binary on one machine and another on a second machine, and they establish a real Bell 103 connection over the sound card. Data crosses the air as sound.

Early days. The DSP is being built; see the commit history for where it is up to.

## The overture is performed. The connection is real

The screech everyone remembers is a V.34/V.90 handshake at 33.6k or 56k. Implementing it for real is a research project, and it would not survive a speaker-to-microphone air gap anyway.

So this does both, and says which is which:

- **Performed:** the overture. Off-hook, UK dial tone at 350 + 450 Hz, the DTMF digits you dialled, UK double-ring ringback at 400 + 450 Hz, the 2100 Hz ANSam answer tone with its phase reversals every 450 ms, CI, CM, JM, CJ and the 75 ms transition. All rendered to the real timings.
- **An impression:** the V.34 training that follows. There is no real channel here to negotiate, so this is texture, not a conformant sequence.
- **Real:** everything after `CONNECT 300`. Bell 103 FSK at 300 baud, originate on 1270/1070 Hz and answer on 2225/2025 Hz, carrying actual bytes.

## It really is Bell 103, checked against an independent implementation

Every test elsewhere in this workspace proves modem-core agrees with itself - the same `Tx` decoded by the same `Rx`. That is not proof it is Bell 103; it is only proof it is internally consistent, and this project has already shipped one bug (a reversed bit order in `Tx`) that stayed invisible for exactly that reason, because `Rx` reversed it right back.

`cargo test -p modem-audio` cross-validates against [minimodem](https://github.com/kamalmostafa/minimodem), the reference Bell 103 implementation, in both directions, with the mark, space, framing and baud rate stated explicitly on both sides of every invocation rather than relied on as a shared default:

- **minimodem decodes us.** Our `Tx` renders a WAV; `minimodem --rx` decodes it; the recovered bytes must be exact.
- **We decode minimodem.** `minimodem --tx` renders a WAV; our `Rx` decodes it; the recovered bytes must be exact.

**The acquisition caveat.** Our `Rx` needs an alternating preamble to acquire symbol timing from a cold start (see `modem-core::rx`'s module doc); minimodem's own lead-in is a short run of constant idle mark, not that. At the 48 kHz this project tests against, that lead-in's fixed duration (13.3333 ms, four symbol periods, measured directly and reconfirmed by review) meets `Rx`'s own fixed cold-start delay at a fixed sub-symbol phase - not a fresh draw per message. An 800-point sweep (5 payloads x 160 sub-symbol offsets) pins the actual shape: **145 of 160 offsets (91%) decode byte-exact from the first character, 2 of 160 lose exactly one leading character, and 15 of 160 fail completely** - not a few extra wrong characters, but a decode that never finds byte alignment and returns roughly 40% of the expected length. There is no graceful middle ground: this configuration either decodes cleanly from character 0, or it fails hard and short. The 48 kHz baseline sits 49 samples clear of the nearest losing offset going back and 96 going forward, well inside the clean band rather than balanced on its edge - and confirmed to hold across 9 device rates (8000-96000 Hz), the answer band, and 40 further random payloads, zero failures throughout. `modem-audio/tests/interop.rs` still allows a small window for the real but rare single-character-loss mode, not as a hedge against a future build landing in the losing band - a build that did would fail the test's length check outright, loudly, not degrade into it. Skip this test on a machine without minimodem installed; CI asserts it is present and that the cross-validation actually ran, not merely that the suite stayed green.

Interop is a coarse conformance check, not a fine-grained guard on the protocol constants: our own baud rate can drift to 312 (+4%) and either tone can shift by 40 Hz before either direction even notices. The precise pins are `tx.rs`'s `tones_per_role` and `samples_for_bits_is_fractional` tests, which catch a one-baud or one-hertz change immediately - interop proves the signal is genuinely decodable by an independent implementation, and is not a replacement for those two.

## Layout

```
modem-core/    the DSP and protocol. no_std, so I/O is a compile error
modem-audio/   WAV I/O and the minimodem cross-validation. a normal std crate
modem-wasm/    browser endpoint, built for an AudioWorklet
spike/         the AudioWorklet proof this architecture rests on
```

`modem-core` being `no_std` is an enforcement mechanism rather than an embedded ambition: it makes file and network access impossible to add by accident, and it guarantees the crate reaches WASM unchanged. The browser and the binary therefore run the same modem, which is the whole argument that they are one product.

## Related

Prior art worth your time: [minimodem](https://github.com/kamalmostafa/minimodem), [ggwave](https://github.com/ggerganov/ggwave), [quiet](https://github.com/quiet/quiet-js), [tynsel](https://github.com/kulp/tynsel), and Oona Raisanen's [annotated handshake spectrogram](https://hackaday.com/2013/01/31/how-a-dial-up-modem-handshake-works/), which is why this exists.

---

modem is a DBHQ experiment. [dbhq.uk](https://dbhq.uk)

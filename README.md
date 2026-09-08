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

**The acquisition caveat.** Our `Rx` needs an alternating preamble to acquire symbol timing from a cold start (see `modem-core::rx`'s module doc); minimodem's own lead-in is a short run of constant idle mark, not that. So the second direction has a real, measured risk of losing the opening characters while the timing loop locks on. Measured against the `minimodem 0.24` this project has tested against, at 48 kHz, across 48 varied trials (different lengths, different leading bit patterns, including random content): **zero characters lost, every time** - the fixed timing this specific combination produces happens to land inside one of the sub-symbol offsets that decode cleanly without a preamble at all. `modem-audio/tests/interop.rs` still allows a small margin above that measured zero rather than asserting bare equality, because the zero is a property of this exact minimodem build and this exact device rate, not a guarantee either side owes the other. Skip this test on a machine without minimodem installed; CI asserts it is present and that the cross-validation actually ran, not merely that the suite stayed green.

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

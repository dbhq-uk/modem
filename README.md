# modem

An acoustic modem simulator. Run one binary on one machine and another on a second machine, and they establish a real Bell 103 connection over the sound card. Data crosses the air as sound.

Early days. The DSP is being built; see `docs/` and the plan for where it is up to.

## The overture is performed. The connection is real

The screech everyone remembers is a V.34/V.90 handshake at 33.6k or 56k. Implementing it for real is a research project, and it would not survive a speaker-to-microphone air gap anyway.

So this does both, and says which is which:

- **Performed:** the overture. Off-hook, UK dial tone at 350 + 450 Hz, the DTMF digits you dialled, UK double-ring ringback at 400 + 450 Hz, the 2100 Hz ANSam answer tone with its phase reversals every 450 ms, CI, CM, JM, CJ and the 75 ms transition. All rendered to the real timings.
- **An impression:** the V.34 training that follows. There is no real channel here to negotiate, so this is texture, not a conformant sequence.
- **Real:** everything after `CONNECT 300`. Bell 103 FSK at 300 baud, originate on 1270/1070 Hz and answer on 2225/2025 Hz, carrying actual bytes.

## Layout

```
modem-core/    the DSP and protocol. no_std, so I/O is a compile error
modem-wasm/    browser endpoint, built for an AudioWorklet
spike/         the AudioWorklet proof this architecture rests on
```

`modem-core` being `no_std` is an enforcement mechanism rather than an embedded ambition: it makes file and network access impossible to add by accident, and it guarantees the crate reaches WASM unchanged. The browser and the binary therefore run the same modem, which is the whole argument that they are one product.

## Related

Prior art worth your time: [minimodem](https://github.com/kamalmostafa/minimodem), [ggwave](https://github.com/ggerganov/ggwave), [quiet](https://github.com/quiet/quiet-js), [tynsel](https://github.com/kulp/tynsel), and Oona Raisanen's [annotated handshake spectrogram](https://hackaday.com/2013/01/31/how-a-dial-up-modem-handshake-works/), which is why this exists.

---

modem is a DBHQ experiment. [dbhq.uk](https://dbhq.uk)

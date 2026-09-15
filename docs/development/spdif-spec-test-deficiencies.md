# S/PDIF decoder: unit-test conformance gaps against EBU Tech 3250

Scope: does the `biphase_audio` engine's unit-test suite guarantee that the
decoder matches the specification? References are the documents in this
directory:

- `tech3250.pdf` — EBU Tech 3250-2004 (the AES/EBU interface)
- `tech3250s1.pdf` — Tech 3250 Supplement 1 (user data channel format)
- `aes-ebu-eg.pdf` — EBU engineering guidelines

Section numbers below refer to Tech 3250 unless stated otherwise. Test and
implementation references are to
`libsurfer/src/decoders/engines/biphase_audio.rs`.

## Answer

**No.** The suite proves that the decoder is self-consistent and robust to
malformed line signals; it does not prove conformance to the standard.
Specifically:

1. The professional channel-status word-length field (byte 2) is decoded
   with the wrong bit order, and the tests encode the same misreading, so
   they pass. A spec-conformant byte value is decoded as the wrong word
   length. This is the same defect found in the card's transmitter (in the
   card repo's `docs/spdif_todo.md`, "Channel-status byte 2 declares a
   reserved state"), and it is a compensating pair: our encoder and our
   decoder agree with each other, not with the document.
2. Most tests build the waveform with the decoder's own encoder helpers
   (`append_preamble`, `append_bits`, `subframe_bits`, `encode_subframe`,
   `encode_block`), so a wrong constant or a wrong reading of a table is
   invisible by construction.
3. Large parts of the standard are not decoded at all (byte 1, byte 3,
   bytes 6–21, byte 22, several byte 0/4 bits), so no test can guarantee
   them.
4. The only externally referenced checks are the CRCC vectors and the
   24-bit slot placement. Everything else is either implementation-derived
   or absent.

## What *is* pinned to the specification

| Requirement | Section | Test | Independent? |
| --- | --- | --- | --- |
| CRCC polynomial and init, both Appendix 1 examples (`0x9B`, `0x32`) | §4, App. 1 | `crc_matches_spec_examples` (`:2149`) | yes — spec vectors |
| 24-bit word: slot 4 LSB, slot 27 MSB | §2.2.1 | `extracts_lsb_first_slots` (`:1783`) | yes — hand-built bit pattern |
| Even parity over slots 4–31 | §2.2.1 | `detects_parity_errors` (`:1816`) | partly — negative case only; clean case implicit in `decodes_stereo_frame` (`:1518`) |
| Block = 192 frames, Z marks the start | §2.1.11, §2.2.2 | `flags_block_count_errors` (`:2289`), `flags_incomplete_final_block`, `keeps_error_row_sorted` | partly — 192 is hard-coded in `encode_block`, not read from the spec |
| Channel A = subframe 1, B = subframe 2 | §2.2.2 | `decodes_stereo_frame` (`:1502`) | no — uses our encoder |
| Extended professional sample-rate codes (byte 4 bits 3–6) | §4 byte 4 | `parses_extended_professional_sample_rates` (`:2265`) | byte values match the table; byte 2 in the same fixture does not |

## Deficiencies

### 1. Professional byte 2 is decoded with the wrong bit order, and the tests lock it in

Spec §4 numbers the bits of a channel-status byte with **bit 0 as the LSB
and the first transmitted**, and the byte 2 tables list the bit states in
that order. The rows are:

```
Bits 0 to 2   Encoded use of auxiliary sample bits
bit  0 1 2
     0 0 0   Max audio sample word length 20 bits (default)
     0 0 1   Max word length 24 bits, aux sample bits used for main audio
     0 1 0   Max word length 20 bits, aux carries a coordination signal
     0 1 1   Reserved for user-defined applications
     others  reserved, "shall not be used"

Bits 3 to 5   Encoded audio sample word length
bit  3 4 5   (max 24)   (max 20)
     0 0 0   not indicated
     0 0 1   23 bits     19 bits
     0 1 0   22 bits     18 bits
     0 1 1   21 bits     17 bits
     1 0 0   20 bits     16 bits
     1 0 1   24 bits     20 bits
     1 1 0 / 1 1 1  reserved
```

A conformant professional transmitter declaring "24-bit range, 24-bit word"
therefore sends byte 2 = `0x2C` (bit 2 = 4, bit 3 = 8, bit 5 = 32), as
recorded in the card repo's `spdif_todo.md`.

The decoder instead treats bit 3 as the field's LSB and bit 0 as the
24-bit flag (`biphase_audio.rs:332-341`):

```rust
let max_24 = (bytes[2] & 0x07) == 0b001;
let word_bits = match (bytes[2] >> 3) & 0x07 {
    1 => 23/19, 2 => 22/18, 3 => 21/17, 4 => 20/16, 5 => 24/20, ...
};
```

Consequences:

| byte 2 | spec says | decoder says |
| --- | --- | --- |
| `0x2C` (bit 2, bit 3, bit 5) | max 24, 24-bit | **20-bit** |
| `0x04` (bit 2) | max 24, word length not indicated → 24-bit | **20-bit** |
| `0x29` (bit 0, bit 3, bit 5) | bits 0–2 = `1 0 0`, reserved | 24-bit |
| `0x20` (bit 5) | max 20, 19-bit | **16-bit** |
| `0x08` (bit 3) | max 20, 16-bit | **19-bit** |
| `0x30` (bit 4, bit 5) | max 20, 17-bit | no word length |

For the bits 3–5 codes, only `2` (`0 1 0`) and `5` (`1 0 1`) happen to
agree; `1` (`1 0 0`) and `4` (`0 0 1`) are swapped, `3` (`1 1 0`) is
reserved but decoded, and `6` (`0 1 1`) is not decoded at all. The bits 0–2
flag checks the wrong bit, so the range is wrong whenever bit 0 and bit 2
disagree.
The wrong `word_bits` feeds `WordBits::resolve`, so `word_bits = auto`
extracts the sample from the wrong slots and the decoded audio is wrong,
not just the summary text.

The three tests that touch professional byte 2 all use the reserved `0x29`
and assert the decoder's own reading:

- `parses_professional_channel_status` (`:2227`): `status[2] = 0b001 |
  (0b101 << 3)` — bits 0–2 = `1 0 0`, reserved — expects `24-bit`.
- `flags_status_crc_errors` (`:2246`): same byte.
- `parses_extended_professional_sample_rates` (`:2265`): same byte.

No test uses `0x2C`, and none derives the expected word length from the
table. The consumer path (`decodes_all_channel_status_word_lengths`,
`:1734`) is a different field (IEC 60958-3 byte 4) and does not cover this.

### 2. Most tests are compensating pairs

The test helpers are the encoder for the same implementation they check:

- Preamble patterns: `PREAMBLES` (`:32`) is used by `append_preamble`
  (`:1269`) to build the waveform and by `match_preamble` to decode it. No
  test asserts the literal eight-state sequences `11100010` / `11100100` /
  `11101000` from §2.4; `matches_complementary_preambles` (`:1802`) only
  proves polarity independence, using the same constant.
- BMC bit encoding: `append_bits` (`:1291`) writes the half cells that the
  decoder's gap logic reads. `decodes_all_word_lengths` (`:1711`),
  `decodes_stereo_frame`, `decodes_many_frames` and the jitter/glitch tests
  all start from this encoder. A wrong encoding rule would be invisible.
- Subframe layout: `subframe_bits` (`:1303`) places the sample, validity,
  user and status bits. The decoder's `extract_audio` (`:916`) is only
  independently checked for the 24-bit case (`extracts_lsb_first_slots`);
  20-bit placement (LSB slot 8) and 16-bit placement (slot 12, by the
  "unused LSB shall be 0" rule) are only checked against `subframe_bits`
  itself.
- Channel-status accumulation: `collect_blocks` (`:415`) and the tests'
  `status_bit`/`encode_block` (`:1341`) share the "first bit after Z is
  byte 0 bit 0" convention. No test builds the 192-bit block independently.
- The example-file tests (`decodes_example_file`, `:2105`) decode a
  Python-generated waveform. That is a second implementation, but it is
  still the project's own encoder and decoder agreeing.

`crc_matches_spec_examples` and `extracts_lsb_first_slots` are the only
tests with an oracle outside the implementation.

### 3. Specification fields that are not decoded

Nothing in the tests can guarantee these, because the decoder has no output
for them:

| Field | Section |
| --- | --- |
| Byte 0 bit 5 (`locked` / sample-frequency-locked) | §4 byte 0 |
| Byte 0 bits 2–4 "no emphasis" state (`1 0 0`) — conflated with "not indicated" | §4 byte 0 |
| Byte 1 bits 0–3 channel mode (two-channel, mono, stereo, double-rate, multichannel) | §4 byte 1 |
| Byte 1 bits 4–7 user-bit management | §4 byte 1 |
| Byte 2 bits 6–7 alignment level | §4 byte 2 |
| Byte 3 channel number / multichannel mode | §4 byte 3 |
| Byte 4 bits 0–1 digital-audio reference grade | §4 byte 4 |
| Byte 5 reserved (should be zero) | §4 byte 5 |
| Bytes 6–9 origin, 10–13 destination (ISO 646) | §4 bytes 6–13 |
| Bytes 14–17 local sample address | §4 bytes 14–17 |
| Bytes 18–21 time-of-day sample address | §4 bytes 18–21 |
| Byte 22 reliability flags | §4 byte 22 |
| Non-zero auxiliary bits when a shorter word is declared (§2.2.1 "unused LSB shall be 0") | §2.2.1 |
| Single-channel mode (subframe 1 only, default to channel 1) | §2.2.2c |
| User data channel format (HDLC packet system) | Supplement 1 |

### 4. Fields that are decoded but not tested

- Byte 0 emphasis: `parse_channel_status` maps `50/15 µs` and `J.17`
  (`:312-316`) but no test exercises either value; `1 0 0` ("no emphasis")
  is not distinguished from `0 0 0` ("not indicated").
- Byte 0 bits 6–7: only the 48 kHz state is tested (`:2229`); 44.1 kHz and
  32 kHz professional states are untested.
- Byte 4 bit 7 scaling flag: decoded into `scaled` and shown as `÷1.001`
  but never asserted.
- Byte 4 reserved codes (4–8, 12–14): silently fall back to byte 0; no test
  records whether that is intended.
- Byte 2 bits 0–2 coordination state (`0 1 0`): ignored; no test.
- Byte 2 reserved states: code `3` (`1 1 0`) is reserved but decoded as
  21/17, and code `6` (`0 1 1`) is valid (21/17) but decoded as no word
  length (see deficiency 1).

### 5. Behavioural requirements with no test

- **Preamble distance from legal biphase.** §2.4 requires preambles to
  differ from any valid biphase sequence by at least two states; the
  decoder's unknown-header classification depends on it. The tests use the
  constant and malform it by one half cell (`malform_preamble`), so the
  distance property itself is not checked.
- **Minimum implementation CRCC.** §5.2.1 notes that a receiver
  implementing byte 23 will report a CRC error when a minimum
  implementation sends the default `0`. There is no test that an all-zero
  block is reported as a CRC error.
- **Single-channel mode** (§2.2.2c) as above.
- **Jitter tolerance.** §6.2.5/§6.3.6 define templates (0.25 UI peak at
  high frequency, 10 UI below 200 Hz). The suite's jitter tests
  (`tolerates_transition_jitter`, `tolerates_transmitter_glitches`) use
  small deterministic offsets and are robustness checks, not the template.

### 6. Consumer mode is outside the provided references

The decoder's consumer path (`:351-399`) follows IEC 60958-3, which is not
in this directory (the README says so). Its tests
(`parses_consumer_channel_status`, `decodes_all_channel_status_word_lengths`)
are derived from the implementation and from ALSA constants, so they cannot
be verified against the references here. Treat consumer conformance as
unproven by this suite.

### 7. Electrical and jitter clauses are out of scope

§6 (line driver/receiver, impedance, amplitude, connectors) describes the
physical interface. A digital waveform decoder cannot test them; the
omission is intentional, not a defect. The same applies to the
engineering-guidelines document.

### 8. User data channel is unimplemented

Supplement 1 defines the U-channel packet format. The decoder exposes raw
user bits (`show_user`, `decodes_validity_and_user_rows`, `:1830`) and
nothing else. The card also transmits U as hard-wired zero (card repo's
`docs/spdif_todo.md`), so there is no end-to-end coverage either.

## Test inventory

| Test | Spec area | Oracle |
| --- | --- | --- |
| `crc_matches_spec_examples` | byte 23, App. 1 | **spec vectors** |
| `extracts_lsb_first_slots` | §2.2.1 24-bit placement | **spec-derived pattern** |
| `detects_parity_errors` | §2.2.1 parity | self |
| `decodes_stereo_frame`, `decodes_many_frames`, `decodes_all_word_lengths` | §2.2.1/2.2.2 | self |
| `matches_complementary_preambles`, preamble label tests | §2.4 | self (constant) |
| `flags_block_count_errors`, `flags_incomplete_final_block`, `keeps_error_row_sorted` | §2.1.11, §2.2.2 | self |
| `parses_professional_channel_status`, `flags_status_crc_errors`, `parses_extended_professional_sample_rates` | §4 bytes 0/2/4 | self, and byte 2 is wrong |
| `parses_consumer_channel_status`, `decodes_all_channel_status_word_lengths`, `resolves_word_bits_from_channel_status` | IEC 60958-3 (absent) | self / ALSA |
| `decodes_validity_and_user_rows` | §2.2.1 slots 28/29 | self |
| `locks_on_mid_frame_start`, `recovers_after_long_lead_in`, `tolerates_undefined_lead_in`, `tolerates_transition_jitter`, `tolerates_transmitter_glitches`, `collapses_simultaneous_transitions`, `recovers_various_half_periods`, `rejects_stream_without_preambles`, `survives_deterministic_mutations`, `recovers_half_period_when_sample_misses_short_gaps` | robustness, §2.3/2.4 | self |
| `reports_word_length_assumption_without_channel_status`, `hides_status_row_when_disabled`, `bundled_schema_loads` | presentation | n/a |

## Recommended actions

1. **Fix byte 2 first, test-first.** Write a test that transcribes the byte 2
   tables as literal rows (`0 0 1` → 23/19, `1 0 0` → 20/16, `0 1 1` →
   21/17, `1 0 1` → 24/20, `0 0 1` on bits 0–2 → max 24), watch it fail,
   then correct `parse_channel_status` and the existing fixtures from `0x29`
   to `0x2C`. Note the card repo's `docs/spdif_todo.md` records the same fix
   on the transmit side, so the two ends can then be checked against each
   other.
2. **Add a spec-vector test module** that does not use `append_bits` /
   `encode_block`: literal half-cell sequences for X/Y/Z, a hand-built
   192-frame block with a known channel status, and hand-placed 16/20/24-bit
   words.
3. **Build the conformance matrix** (the card repo's phase 3): walk §2.2,
   §2.4 and §4 field by field and record decoded / not decoded / deliberate.
   The table in deficiency 3 is a starting point.
4. **Record the consumer and out-of-scope decisions** so the omissions are
   deliberate rather than invisible.

# Signal decoders

Signal decoders turn a set of related signals (a clock, a frame sync, a serial
data line, ...) into one multi-row waveform item: one row per channel with the
decoded values, plus optional status and error rows. Rows can be rendered as
sample cells, as an analog waveform, or both.

Unlike [translator decoders](decoders.md), which map a single variable to a
format, signal decoders read several signals and produce their own rows.

## Built-in decoders

| Id | Name | Inputs | Description |
|---|---|---|---|
| `tdm_audio` | TDM Audio | `bitclk`, `frame_sync`, `data` | Time-division-multiplexed audio (TDM) and I2S, one row per channel |
| `spdif` | S/PDIF / AES3 | `data`, optional `bitclk` | Biphase-mark-coded consumer S/PDIF and professional AES3, including channel status |

### TDM Audio (`tdm_audio`)

Decodes a serial data line sampled on `bitclk`, with `frame_sync` marking the
start of a frame.

| Setting | Type | Default | Description |
|---|---|---|---|
| `mode` | `pulse` / `level` | `pulse` | `pulse` for a one-clock-wide TDM sync, `level` for an I2S word-select |
| `justification` | `left` / `right` | `left` | Where the sample sits in its slot |
| `edge` | `rising` / `falling` | `rising` | `bitclk` edge that samples `data` |
| `fs_edge` | `rising` / `falling` | `rising` | `frame_sync` edge that starts a frame |
| `active_high` | bool | `true` | Polarity of the active frame-sync level (`level` mode; `pulse` uses `fs_edge`) |
| `bits` | integer 1-64 | `16` | Bits per sample |
| `channels` | integer 1-64 | `2` | Channels per frame |
| `slot_width` | integer 0-64 | `0` | Slot width; `0` uses `bits` |
| `offset` | integer 0-64 | `0` | Delay from the frame sync edge to the start of the slot window (the sample is justified within it) |
| `signed` | bool | `true` | Two's-complement samples |
| `show_errors` | bool | `true` | Add an error row for frames that end before every configured channel is read |
| `order` | `msb` / `lsb` | `msb` | Bit order on the wire |

If the frame sync period is too short for the configured `channels`, `bits`,
and `slot_width`, the samples that do not fit are dropped and the error row
reports how many channels of the frame were read. Level (`level`) mode reports
a sample that does not complete before the next word-select edge instead.
At most 100 frame errors are reported per decode; any further errors are
replaced by a single `further frame errors suppressed` marker.

### S/PDIF / AES3 (`spdif`)

Decodes a biphase-mark-coded line. The bit period is recovered from the line
transitions, tolerating transition jitter, half-period edge glitches from
clocked transmitters, idle lead-ins before the first frame, and simultaneous
duplicate events by retaining their final line state. Each subframe is
labelled `Preamble M`, `Preamble W`, or `Preamble B`; an unrecognized framing
header is labelled `Unknown Preamble` and also reported as an error. The
optional `bitclk` input can be assigned to sample `data` directly when a bit
clock is available.

| Setting | Type | Default | Description |
|---|---|---|---|
| `word_bits` | `auto` / `16` / `20` / `24` | `auto` | Audio word length; `auto` uses the channel status |
| `signed` | bool | `true` | Two's-complement samples |
| `show_validity` | bool | `false` | Add a validity-bit row |
| `show_user` | bool | `false` | Add a user-bit row |
| `show_status` | bool | `true` | Add a parsed channel-status row |
| `show_parity_errors` | bool | `true` | Add an error row (parity, block-count, and unrecognized-preamble errors) |
| `show_preambles` | bool | `true` | Add a per-subframe preamble row |

## Using the GUI

Select **View → Add decoder…**, or right-click a variable and pick
**Add decoder…**. Choose the decoder type, assign the inputs, adjust the
settings, and optionally enable sample values and the analog waveform.
The item's settings entry opens the dialog again to edit an existing decoder.
The add dialog lists all inputs, including optional ones, so they can be
assigned while adding. The edit dialog only shows optional inputs that are
already assigned.

## Commands

Positional inputs are given in the order the decoder declares them:

``` text
decoder_add tdm_audio tb.bitclk tb.frame_sync tb.data channels=2 bits=16
decoder_add spdif tb.data analog=interpolated
decoder_add spdif tb.data tb.bitclk word_bits=24 format=hex
```

`decoder_add` accepts the decoder id, one signal per input, and any settings as
`key=value`. Display options can be set in the same command:

| Option | Values | Description |
|---|---|---|
| `samples` | `on` / `off` | Show sample-value cells |
| `format` | `dec` / `hex` | Sample value format |
| `analog` | `off` / `step` / `interpolated` | Analog rendering |
| `analog_scale` | `viewport` / `global` / `type_limits` | Analog Y-axis scaling |

With a decoder item selected, these commands update its display:

``` text
item_set_samples on|off
item_set_value_format dec|hex
item_set_analog off|step|interpolated
```

## User-defined schemas

Additional signal decoders can be defined without writing Rust by placing a
TOML schema in a `signal_decoders` directory:

| Os      | Path                                                                  |
|---------|-----------------------------------------------------------------------|
| Linux   | `~/.config/surfer/signal_decoders/`                                   |
| Windows | `C:\Users\<Name>\AppData\Roaming\surfer-project\surfer\config\signal_decoders\` |
| macOS   | `/Users/<Name>/Library/Application Support/org.surfer-project.surfer/signal_decoders/` |

Schemas in a `.surfer/signal_decoders/` directory are also loaded. The same
search as for the configuration is used: the current directory and all of its
parents are checked, which is useful for project-specific protocols.

A schema selects a built-in decoding *engine* and maps its parameters to
user-visible settings:

``` toml
id = "my_tdm"                     # unique id used by decoder_add
name = "My TDM bus"               # name shown in the GUI
engine = "clocked_serial"         # decoding engine

[[inputs]]
role = "bitclk"
description = "Bit clock"

[[inputs]]
role = "frame_sync"
description = "Frame sync"

[[inputs]]
role = "data"
description = "Serial data"
required = false                  # optional inputs may be left unassigned

[[settings]]
key = "channels"
label = "Channels"
type = "integer"                  # bool, integer, or enum
default = 8
min = 1                           # integer settings only
max = 64

[[settings]]
key = "order"
label = "Bit order"
type = "enum"
default = "msb"
options = [
    { value = "msb", label = "MSB first" },
    { value = "lsb", label = "LSB first" },
]

[engine_params]
clock = "bitclk"                  # literal engine parameter
data = "data"
frame = "frame_sync"
channels = "$channels"            # `$key` resolves to a setting value
bit_order = "$order"
```

### Engines

| Engine | Parameters |
|---|---|
| `clocked_serial` | `clock`, `clock_edge`, `data`, `frame`, `frame_mode`, `frame_edge`, `frame_active_high`, `bits`, `channels`, `bit_order`, `justification`, `offset`, `signed`, `show_errors`, `slot_width` |
| `biphase_audio` | `data`, `bitclk`, `word_bits`, `signed`, `show_validity`, `show_user`, `show_status`, `show_parity_errors`, `show_preambles` |

Engine parameters are the keys in `[engine_params]`. A value that starts with
`$` refers to a setting key; any other value is a literal.

Enum settings must declare their `options`; the stored value is the option's
`value`. Integer settings may set `min` and `max` bounds.

# Audio Assets

Runtime audio is loaded by `crates/riverborn/src/main.rs`.

Expected files:

- `bgm.ogg` - looping background music.
- `check.ogg` - played for Check and Call.
- `fold.ogg` - played for Fold.
- `raise.ogg` - played for Raise and All-in.

Use OGG Vorbis for the safest Bevy compatibility. Keep SFX short, roughly 0.1-0.5s.

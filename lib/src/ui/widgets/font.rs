use embedded_graphics::mono_font::MonoFont;
use embedded_graphics_unicodefonts::MONO_9X18;

/// Static font. Do not use `mono_9x18_atlas()` here:
/// it `Box::leak`s a `FontAtlas` and puts a `dyn GlyphMapping` into `MonoFont`.
/// Caching that struct in `.bss` leaves a dangling vtable — on real HW that
/// shows up as `EIP=CR2=0xc000` on the first text draw.
pub fn font() -> &'static MonoFont<'static> {
    &MONO_9X18
}

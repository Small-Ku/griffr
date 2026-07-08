use std::collections::HashMap;

use winio::prelude::{Color, DrawingFont, HAlign, SolidColorBrush, VAlign};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FontKey {
    family: String,
    size_bits: u64,
    italic: bool,
    bold: bool,
    halign: AlignKey,
    valign: AlignKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AlignKey {
    Left,
    Center,
    Right,
    Stretch,
    Top,
    Bottom,
}

impl AlignKey {
    fn from_halign(value: HAlign) -> Self {
        match value {
            HAlign::Left => Self::Left,
            HAlign::Center => Self::Center,
            HAlign::Right => Self::Right,
            HAlign::Stretch => Self::Stretch,
        }
    }

    fn from_valign(value: VAlign) -> Self {
        match value {
            VAlign::Top => Self::Top,
            VAlign::Center => Self::Center,
            VAlign::Bottom => Self::Bottom,
            VAlign::Stretch => Self::Stretch,
        }
    }
}

impl From<&DrawingFont> for FontKey {
    fn from(value: &DrawingFont) -> Self {
        Self {
            family: value.family.clone(),
            size_bits: value.size.to_bits(),
            italic: value.italic,
            bold: value.bold,
            halign: AlignKey::from_halign(value.halign),
            valign: AlignKey::from_valign(value.valign),
        }
    }
}

#[derive(Default)]
pub struct DrawResources {
    solid_brushes: HashMap<[u8; 4], SolidColorBrush>,
    fonts: HashMap<FontKey, DrawingFont>,
}

impl DrawResources {
    pub fn clear(&mut self) {
        self.solid_brushes.clear();
        self.fonts.clear();
    }

    pub fn solid_brush(&mut self, color: Color) -> SolidColorBrush {
        self.solid_brushes
            .entry([color.r, color.g, color.b, color.a])
            .or_insert_with(|| SolidColorBrush::new(color))
            .clone()
    }

    pub fn font(&mut self, font: DrawingFont) -> DrawingFont {
        let key = FontKey::from(&font);
        self.fonts.entry(key).or_insert(font).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::DrawResources;
    use winio::prelude::{Color, DrawingFontBuilder};

    #[test]
    fn reuses_cached_solid_brushes_and_fonts() {
        let mut resources = DrawResources::default();

        let brush_a = resources.solid_brush(Color::new(1, 2, 3, 4));
        let brush_b = resources.solid_brush(Color::new(1, 2, 3, 4));
        assert_eq!(brush_a.color, brush_b.color);
        assert_eq!(resources.solid_brushes.len(), 1);

        let mut builder = DrawingFontBuilder::new();
        let font = builder.family("Segoe UI").size(12.0).build();
        let font_a = resources.font(font.clone());
        let font_b = resources.font(font);
        assert_eq!(font_a.family, font_b.family);
        assert_eq!(font_a.size.to_bits(), font_b.size.to_bits());
        assert_eq!(resources.fonts.len(), 1);
    }

    #[test]
    fn clear_drops_cached_entries() {
        let mut resources = DrawResources::default();
        resources.solid_brush(Color::new(1, 2, 3, 4));

        let mut builder = DrawingFontBuilder::new();
        resources.font(builder.family("Segoe UI").size(12.0).build());
        resources.clear();

        assert!(resources.solid_brushes.is_empty());
        assert!(resources.fonts.is_empty());
    }
}

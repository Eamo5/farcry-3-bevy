use crate::character::Character;
use crate::context::{RenderContext, UpdateContext};
use crate::html::layout::InlineImage;
use crate::library::Library;
use crate::string::WStr;
use crate::font::{DefaultFont, Font, FontType};
use gc_arena::Mutation;
use ruffle_common::tag_utils::SwfMovie;

/// Provides access to the font resolution operations needed to lay out text,
/// common to both `UpdateContext` and `RenderContext`.
pub trait LayoutContext<'gc> {
    /// Retrieve the current GC context.
    fn gc(&self) -> &'gc Mutation<'gc>;

    /// Find an embedded font by its name and parameters.
    fn get_embedded_font_by_name(
        &self,
        name: &str,
        font_type: FontType,
        is_bold: bool,
        is_italic: bool,
        movie: Option<SwfMovie<'gc>>,
    ) -> Option<Font<'gc>>;

    /// Returns the default font implementations behind a built-in name.
    fn default_font(&mut self, name: DefaultFont, is_bold: bool, is_italic: bool)
    -> Vec<Font<'gc>>;

    /// Returns the device font(s) best matching the given name and parameters.
    fn get_or_sort_device_fonts(
        &mut self,
        name: &str,
        is_bold: bool,
        is_italic: bool,
    ) -> Vec<Font<'gc>>;

    /// fc3 patch: an image inline in text (`<img src="...">`), the symbol
    /// the movie exports under that name.
    fn inline_image(&self, src: &WStr, id: Option<&WStr>, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>>;
}

/// fc3 patch: an exported sprite's first-frame shapes (as Scaleform draws
/// `<img>` in text: inline, the symbol at its own size), or an exported
/// shape alone. With an `id` (the clip's instance name, as Scaleform makes
/// it), the clip's text fields read it: the PC's keycaps (`PCKeySmallMC`)
/// name their key by the clip (`PCKeyTxt.text = _parent._name`).
fn inline_image<'gc>(fonts: &Library<'gc>, src: &WStr, id: Option<&WStr>, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>> {
    let library = fonts.library_for_movie(movie)?;
    let (graphics, fields) = match library.character_by_export_name(src)?.1 {
        Character::MovieClip(clip) => (clip.first_frame_graphics(library), clip.first_frame_texts(library)),
        Character::Graphic(graphic) => (vec![(graphic, Default::default())], Vec::new()),
        _ => return None,
    };
    let texts = id.map_or_else(Vec::new, |id| fields.into_iter().filter_map(|(field, matrix)| inline_text(fonts, field, matrix, id, movie)).collect());
    InlineImage::new(graphics, texts)
}

/// fc3 patch: a text field's word in an inline image, laid out as the field
/// would: its format's font, size and colour, aligned in its bounds.
fn inline_text<'gc>(
    fonts: &Library<'gc>,
    field: crate::display_object::EditText<'gc>,
    matrix: ruffle_render::matrix::Matrix,
    text: &WStr,
    movie: SwfMovie<'gc>,
) -> Option<crate::html::layout::InlineText<'gc>> {
    use crate::font::{EvalParameters, FontLike};
    use swf::Twips;
    let format = field.new_text_format();
    let name = format.font.as_ref()?.to_utf8_lossy().into_owned();
    let (bold, italic) = (format.bold.unwrap_or(false), format.italic.unwrap_or(false));
    // (As the field asks, else bold or plain, the movie's or any loaded.)
    let font = [(bold, italic), (true, false), (false, false)]
        .into_iter()
        .flat_map(|(b, i)| [Some(movie), None].map(|m| (b, i, m)))
        .find_map(|(b, i, m)| fonts.get_embedded_font_by_name(&name, FontType::Embedded, b, i, m));
    let font = font?;
    let params = EvalParameters {
        height: Twips::from_pixels(format.size.unwrap_or(12.0)),
        letter_spacing: Twips::from_pixels(format.letter_spacing.unwrap_or(0.0)),
        kerning: format.kerning.unwrap_or(false),
        // (A format's colour carries no alpha.)
        color: swf::Color::from_rgb(format.color.unwrap_or(swf::Color::BLACK).to_rgb(), 0xFF),
    };
    let bounds = field.fc3_bounds();
    let gutter = Twips::from_pixels(2.0);
    let room = bounds.width() - gutter - gutter;
    let width = font.measure(text, params);
    let x = match format.align {
        Some(swf::TextAlign::Center) => (room - width) / 2,
        Some(swf::TextAlign::Right) => room - width,
        _ => Twips::ZERO,
    };
    let at = ruffle_render::matrix::Matrix::translate(bounds.x_min + gutter + x, bounds.y_min + gutter);
    Some(crate::html::layout::InlineText { font, text: text.to_owned(), params, matrix: matrix * at })
}

impl<'gc> LayoutContext<'gc> for UpdateContext<'gc> {
    #[inline(always)]
    fn gc(&self) -> &'gc Mutation<'gc> {
        self.gc_context
    }

    fn get_embedded_font_by_name(
        &self,
        name: &str,
        font_type: FontType,
        is_bold: bool,
        is_italic: bool,
        movie: Option<SwfMovie<'gc>>,
    ) -> Option<Font<'gc>> {
        self.library
            .get_embedded_font_by_name(name, font_type, is_bold, is_italic, movie)
    }

    fn default_font(
        &mut self,
        name: DefaultFont,
        is_bold: bool,
        is_italic: bool,
    ) -> Vec<Font<'gc>> {
        self.library.default_font(
            name,
            is_bold,
            is_italic,
            self.ui,
            self.renderer,
            self.gc_context,
        )
    }

    fn get_or_sort_device_fonts(
        &mut self,
        name: &str,
        is_bold: bool,
        is_italic: bool,
    ) -> Vec<Font<'gc>> {
        self.library.get_or_sort_device_fonts(
            name,
            is_bold,
            is_italic,
            self.ui,
            self.renderer,
            self.gc_context,
        )
    }

    fn inline_image(&self, src: &WStr, id: Option<&WStr>, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>> {
        inline_image(self.library, src, id, movie)
    }
}

impl<'gc> LayoutContext<'gc> for RenderContext<'_, 'gc> {
    #[inline(always)]
    fn gc(&self) -> &'gc Mutation<'gc> {
        self.gc_context
    }

    fn get_embedded_font_by_name(
        &self,
        name: &str,
        font_type: FontType,
        is_bold: bool,
        is_italic: bool,
        movie: Option<SwfMovie<'gc>>,
    ) -> Option<Font<'gc>> {
        self.library
            .get_embedded_font_by_name(name, font_type, is_bold, is_italic, movie)
    }

    fn default_font(
        &mut self,
        name: DefaultFont,
        is_bold: bool,
        is_italic: bool,
    ) -> Vec<Font<'gc>> {
        self.library.default_font(
            name,
            is_bold,
            is_italic,
            self.ui,
            self.renderer,
            self.gc_context,
        )
    }

    fn get_or_sort_device_fonts(
        &mut self,
        name: &str,
        is_bold: bool,
        is_italic: bool,
    ) -> Vec<Font<'gc>> {
        self.library.get_or_sort_device_fonts(
            name,
            is_bold,
            is_italic,
            self.ui,
            self.renderer,
            self.gc_context,
        )
    }

    fn inline_image(&self, src: &WStr, id: Option<&WStr>, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>> {
        inline_image(self.library, src, id, movie)
    }
}

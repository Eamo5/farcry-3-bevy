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
    fn inline_image(&self, src: &WStr, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>>;
}

/// fc3 patch: an exported sprite's first-frame shapes (as Scaleform draws
/// `<img>` in text: inline, the symbol at its own size), or an exported
/// shape alone.
fn inline_image<'gc>(library: &Library<'gc>, src: &WStr, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>> {
    let library = library.library_for_movie(movie)?;
    let graphics = match library.character_by_export_name(src)?.1 {
        Character::MovieClip(clip) => clip.first_frame_graphics(library),
        Character::Graphic(graphic) => vec![(graphic, Default::default())],
        _ => return None,
    };
    InlineImage::new(graphics)
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

    fn inline_image(&self, src: &WStr, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>> {
        inline_image(self.library, src, movie)
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

    fn inline_image(&self, src: &WStr, movie: SwfMovie<'gc>) -> Option<InlineImage<'gc>> {
        inline_image(self.library, src, movie)
    }
}

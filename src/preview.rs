//! Native image previews and per-pane Markdown preview routing.
use super::*;
use scarlet_ui::{
    element::{ElementRenderObject, LayoutConstraints, RenderElement, UpdateResult},
    event::MouseEvent,
    renderer::PaintContext,
};
use std::any::Any;

impl App {
    pub(super) fn is_image(&self, id: DocumentId) -> bool {
        self.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .is_some_and(|doc| doc.image.is_some())
    }
    pub(super) fn document_view(&self, id: DocumentId, group: usize) -> impl View + Clone + use<> {
        let image = self
            .runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .and_then(|doc| doc.image.clone());
        if let Some(tab) = self.preview_tab(group) {
            Either::A(self.markdown_view(tab, group))
        } else if let Some(image) = image {
            Either::B(Either::A(self.image_view(id, group, image)))
        } else {
            Either::B(Either::B(self.editor_view(id, group)))
        }
    }
    fn set_image_zoom(&self, id: DocumentId, group: usize, zoom: f32) {
        self.runtime
            .lock()
            .unwrap()
            .image_zoom
            .insert((id, group), zoom);
        self.changed();
    }
    fn image_view(
        &self,
        id: DocumentId,
        group: usize,
        asset: Arc<kureha_code::media::ImageAsset>,
    ) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let zoom = self
            .runtime
            .lock()
            .unwrap()
            .image_zoom
            .get(&(id, group))
            .copied()
            .unwrap_or(0.0);
        let control = |label: &str, value: f32| {
            let app = self.clone();
            Button::new(label)
                .header_style()
                .font_size(11.0)
                .padding(3.0)
                .text_color(colors.foreground)
                .background_color(if zoom == value {
                    colors.selection
                } else {
                    Color::TRANSPARENT
                })
                .on_click(move || app.set_image_zoom(id, group, value))
        };
        let picture = Image::from_bitmap(asset.bitmap.clone());
        let body = if zoom == 0.0 {
            Either::A(picture.frame(f32::INFINITY, f32::INFINITY))
        } else {
            let w = asset.bitmap.width() as f32 * zoom;
            let h = asset.bitmap.height() as f32 * zoom;
            Either::B(
                ScrollView::new(picture.frame(w, h))
                    .both_axes()
                    .content_size(w, h)
                    .frame(f32::INFINITY, f32::INFINITY),
            )
        };
        let focus = self.clone();
        Surface::section(vstack! {
            hstack! {
                control("Fit", 0.0), control("100%", 1.0),
                control("−", (if zoom == 0.0 {1.0} else {zoom} / 1.5).max(0.1)),
                Text::new(if zoom == 0.0 {"Auto".into()} else {format!("{:.0}%", zoom * 100.0)})
                    .font_size(11.0).color(colors.muted),
                control("+", (if zoom == 0.0 {1.0} else {zoom} * 1.5).min(8.0)),
                Spacer::new(),
            }.spacing(2.0).padding(6.0).clip(),
            Mat { child: body, background: theme.editor_colors().background, tile: colors.muted.with_opacity(0.10) }
                .frame(f32::INFINITY, f32::INFINITY).clip(),
        }.spacing(0.0).frame(f32::INFINITY, f32::INFINITY))
            .fill(theme.editor_colors().background)
            .corner_radius(12.0)
            .clip_content(true)
            .padding_insets(EdgeInsets::new(4.0, 0.0, 4.0, 4.0))
            .on_event_capture(move |event| {
                if matches!(event, Event::Mouse(MouseEvent::ButtonPressed { .. })) { focus.focus_group(group); }
                false
            })
    }
}

#[derive(Clone)]
struct Mat<V> {
    child: V,
    background: Color,
    tile: Color,
}
impl<V: View + Clone> View for Mat<V> {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(RenderElement::with_view_children_and_updater(
            self.clone(),
            |v| MatRenderer {
                size: Size::ZERO,
                background: v.background,
                tile: v.tile,
            },
            |r, v| {
                r.background = v.background;
                r.tile = v.tile;
                UpdateResult::Updated
            },
            |v| vec![v.child.clone_view()],
        ))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
struct MatRenderer {
    size: Size,
    background: Color,
    tile: Color,
}
impl ElementRenderObject for MatRenderer {
    fn layout(&mut self, _: LayoutConstraints) -> Size {
        self.size
    }
    fn layout_with_children(
        &mut self,
        c: LayoutConstraints,
        children: &mut [Box<dyn Element>],
    ) -> Size {
        self.size = children[0].layout(c);
        self.size
    }
    fn size(&self) -> Size {
        self.size
    }
    fn render(&mut self) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn paint<'a>(&'a self, ctx: &mut PaintContext<'a>, origin: Point) -> bool {
        ctx.fill_rect(Rect::new(origin, self.size), self.background);
        let pitch = 16.0;
        for y in 0..(self.size.height / pitch).ceil() as usize {
            for x in 0..(self.size.width / pitch).ceil() as usize {
                if (x + y) % 2 == 0 {
                    ctx.fill_rect(
                        Rect::from_xywh(
                            origin.x + x as f32 * pitch,
                            origin.y + y as f32 * pitch,
                            pitch.min(self.size.width - x as f32 * pitch),
                            pitch.min(self.size.height - y as f32 * pitch),
                        ),
                        self.tile,
                    );
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use scarlet_ui::pipeline::RenderingPipeline;

    fn png() -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..200 {
            for x in 0..320 {
                let color = if x < 30 || x > 289 || y < 30 || y > 169 {
                    [0, 0, 0, 0]
                } else if x < 160 {
                    [220, 70, 105, 220]
                } else {
                    [50, 160, 180, 180]
                };
                pixels.extend_from_slice(&color);
            }
        }
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&pixels, 320, 200, image::ExtendedColorType::Rgba8)
            .unwrap();
        bytes
    }

    #[test]
    fn worker_opens_images_read_only_and_zoom_survives_pane_moves() {
        let app = App::with_document();
        let directory =
            std::env::temp_dir().join(format!("kureha-image-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("alpha.png");
        let bytes = png();
        std::fs::write(&path, &bytes).unwrap();
        app.open(path.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.runtime.lock().unwrap().job.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "image worker timed out"
            );
            app.poll();
            thread::sleep(std::time::Duration::from_millis(5));
        }
        let id = app.document_id();
        assert!(app.is_image(id));
        assert!(!app.editable(id));
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "do not overwrite image".into(),
            },
        );
        app.save_to(id, path.clone());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            !app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(id)
                .unwrap()
                .editor
                .dirty()
        );
        app.set_image_zoom(id, 0, 2.0);
        app.split_tab(id, 0, tab_drag::Edge::Right);
        assert_eq!(app.runtime.lock().unwrap().image_zoom[&(id, 1)], 2.0);
        app.move_tab(id, 1, 0, 0);
        assert_eq!(app.runtime.lock().unwrap().image_zoom[&(id, 0)], 2.0);
        let window = || {
            Window::new("Image", app.content())
                .decorated(false)
                .size(Size::new(900.0, 600.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        assert!(pipeline.render().is_some());
        app.set_image_zoom(id, 0, 0.0);
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        assert!(pipeline.render().is_some());
        pipeline.teardown();
        app.remove_document(id);
        assert!(
            !app.runtime
                .lock()
                .unwrap()
                .image_zoom
                .keys()
                .any(|(document, _)| *document == id)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn narrow_preview_contains_controls_and_clips_enlarged_images() {
        fn controls(element: &dyn Element, parent: Point, output: &mut Vec<Rect>) {
            let origin = Point::new(
                parent.x + element.position().x,
                parent.y + element.position().y,
            );
            if element
                .render_object()
                .is_some_and(|r| r.as_any().is::<scarlet_ui::views::ButtonRenderObject>())
            {
                output.push(Rect::new(origin, element.bounds().size));
            }
            for child in element.children() {
                controls(child.as_ref(), origin, output);
            }
        }
        let app = App::with_document();
        let id = app.document_id();
        let image = Arc::new(kureha_code::media::decode(&png()).unwrap());
        let background = Color::rgb(17, 29, 43);
        for width in [180.0, 260.0] {
            for zoom in [0.0, 1.0, 8.0] {
                app.set_image_zoom(id, 0, zoom);
                let mut pipeline = RenderingPipeline::new();
                pipeline.set_root(
                    Window::new(
                        "Clipping",
                        app.image_view(id, 0, image.clone())
                            .frame(width, 220.0)
                            .padding(20.0)
                            .background(background),
                    )
                    .decorated(false)
                    .size(Size::new(width + 40.0, 260.0))
                    .create_element(),
                );
                pipeline.layout_initial();
                let mut buttons = Vec::new();
                controls(
                    pipeline.element_tree().root().unwrap(),
                    Point::ZERO,
                    &mut buttons,
                );
                assert_eq!(buttons.len(), 4);
                for button in buttons {
                    assert!(
                        button.origin.x >= 24.0
                            && button.origin.x + button.size.width <= width + 16.0,
                        "control outside narrow preview: {button:?}, width={width}"
                    );
                }
                let buffer = pipeline.render().unwrap();
                let pixel = |x: usize, y: usize| {
                    buffer.as_slice()[y * buffer.width() as usize + x] & 0xffffff
                };
                // The 4px inset and outer gutter must survive Fit and 800% zoom.
                for y in 80..230 {
                    for x in [19, 20, 21, width as usize + 18, width as usize + 20] {
                        assert_eq!(
                            pixel(x, y),
                            0x111d2b,
                            "preview leaked at {x},{y}, zoom={zoom}"
                        );
                    }
                }
                pipeline.teardown();
            }
        }
    }

    #[test]
    #[ignore = "manual preview; writes KUREHA_IMAGE_PREVIEW PPM"]
    fn image_preview_screenshot() {
        use std::io::Write;
        let app = App::with_document();
        app.complete_open(
            0,
            PathBuf::from("/preview/alpha.png"),
            String::new(),
            Some(kureha_code::media::decode(&png()).unwrap()),
        );
        app.update_position();
        app.poll_language();
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Image preview", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 720.0))
                .create_element(),
        );
        pipeline.layout_initial();
        let buffer = pipeline.render().unwrap();
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(std::env::var_os("KUREHA_IMAGE_PREVIEW").unwrap()).unwrap(),
        );
        write!(file, "P6\n{} {}\n255\n", buffer.width(), buffer.height()).unwrap();
        for pixel in buffer.as_slice() {
            file.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8])
                .unwrap();
        }
        pipeline.teardown();
    }
}

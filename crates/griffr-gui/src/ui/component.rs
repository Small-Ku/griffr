use winio::prelude::*;

use crate::ui::widget::{Banner, Button, Container, TileSlot, Widget};
use crate::ui::{CanvasEvent, UiRuntime, WidgetId};

const COMPONENT_OVERDRAW_PX: f64 = 0.5;
const TILE_OVERLAP_PX: f64 = 0.5;

pub struct UiComponent {
    root: Child<View>,
    tile0: Child<Canvas>,
    tile1: Child<Canvas>,
    tile2: Child<Canvas>,
    tile3: Child<Canvas>,
    tile4: Child<Canvas>,
    tile5: Child<Canvas>,
    tile6: Child<Canvas>,
    tile7: Child<Canvas>,
    tile8: Child<Canvas>,
    runtime: UiRuntime,
    widgets: Vec<(WidgetId, Box<dyn Widget>)>,
    pointers: [Point; 9],
}

#[derive(Debug)]
pub enum UiMessage {
    Noop,
    Resize(Size),
    Canvas(usize, CanvasEvent),
}

#[derive(Debug)]
pub enum UiEvent {
    Redraw,
    Target(Option<WidgetId>),
}

impl Component for UiComponent {
    type Error = Error;
    type Event = UiEvent;
    type Init<'a> = (&'a Child<Window>, UiRuntime);
    type Message = UiMessage;

    async fn init(init: Self::Init<'_>, _sender: &ComponentSender<Self>) -> Result<Self> {
        let (window, runtime) = init;
        init! {
            root: View = (window),
            tile0: Canvas = (&root),
            tile1: Canvas = (&root),
            tile2: Canvas = (&root),
            tile3: Canvas = (&root),
            tile4: Canvas = (&root),
            tile5: Canvas = (&root),
            tile6: Canvas = (&root),
            tile7: Canvas = (&root),
            tile8: Canvas = (&root),
        }
        let size = window.client_size()?;
        root.set_loc(Point::new(0.0, 0.0))?;
        root.set_size(expand_size(size))?;
        Ok(Self {
            root,
            tile0,
            tile1,
            tile2,
            tile3,
            tile4,
            tile5,
            tile6,
            tile7,
            tile8,
            widgets: Self::build_widgets(&runtime)?,
            runtime,
            pointers: [Point::new(0.0, 0.0); 9],
        })
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        start! {
            sender, default: UiMessage::Noop,
            self.tile0 => {
                e => UiMessage::Canvas(0, e),
            },
            self.tile1 => {
                e => UiMessage::Canvas(1, e),
            },
            self.tile2 => {
                e => UiMessage::Canvas(2, e),
            },
            self.tile3 => {
                e => UiMessage::Canvas(3, e),
            },
            self.tile4 => {
                e => UiMessage::Canvas(4, e),
            },
            self.tile5 => {
                e => UiMessage::Canvas(5, e),
            },
            self.tile6 => {
                e => UiMessage::Canvas(6, e),
            },
            self.tile7 => {
                e => UiMessage::Canvas(7, e),
            },
            self.tile8 => {
                e => UiMessage::Canvas(8, e),
            }
        }
    }

    async fn update_children(&mut self) -> Result<bool> {
        update_children!(
            self.root, self.tile0, self.tile1, self.tile2, self.tile3, self.tile4, self.tile5,
            self.tile6, self.tile7, self.tile8
        )
    }

    async fn update(
        &mut self,
        message: Self::Message,
        sender: &ComponentSender<Self>,
    ) -> Result<bool> {
        match message {
            UiMessage::Noop => Ok(false),
            UiMessage::Resize(size) => {
                self.root.set_loc(Point::new(0.0, 0.0))?;
                self.root.set_size(expand_size(size))?;
                Ok(true)
            }
            UiMessage::Canvas(idx, ev) => {
                match ev {
                    CanvasEvent::MouseMove(p) => {
                        if idx < self.pointers.len() {
                            self.pointers[idx] = self.local_to_global(idx, p);
                        }
                    }
                    _ => {}
                }
                let p = self
                    .pointers
                    .get(idx)
                    .copied()
                    .unwrap_or(Point::new(0.0, 0.0));
                let hit = self.runtime.dispatch_with_pointer(&ev, p.x, p.y);
                if let Some(hit_id) = hit {
                    if let Some((_, widget)) = self.widgets.iter_mut().find(|(id, _)| *id == hit_id)
                    {
                        widget.handle_event(&ev)?;
                    }
                }
                sender.output(UiEvent::Target(hit));
                sender.output(UiEvent::Redraw);
                Ok(true)
            }
        }
    }

    fn render(&mut self, _sender: &ComponentSender<Self>) -> Result<()> {
        let size = self.root.size()?;
        self.runtime.relayout(size);
        let max_right = self
            .runtime
            .plan
            .tile_plan
            .tiles
            .iter()
            .map(|t| t.bounds.x + t.bounds.w)
            .fold(0.0f64, f64::max);
        let max_bottom = self
            .runtime
            .plan
            .tile_plan
            .tiles
            .iter()
            .map(|t| t.bounds.y + t.bounds.h)
            .fold(0.0f64, f64::max);
        for idx in 0..9 {
            let canvas = match idx {
                0 => &mut self.tile0,
                1 => &mut self.tile1,
                2 => &mut self.tile2,
                3 => &mut self.tile3,
                4 => &mut self.tile4,
                5 => &mut self.tile5,
                6 => &mut self.tile6,
                7 => &mut self.tile7,
                _ => &mut self.tile8,
            };
            if let Some(tile) = self.runtime.plan.tile_plan.tiles.get(idx) {
                let mut draw_w = tile.bounds.w;
                let mut draw_h = tile.bounds.h;
                // if tile.bounds.x + tile.bounds.w < max_right - 0.001 {
                draw_w += TILE_OVERLAP_PX;
                // }
                // if tile.bounds.y + tile.bounds.h < max_bottom - 0.001 {
                draw_h += TILE_OVERLAP_PX;
                // }
                canvas.set_visible(true)?;
                canvas.set_loc(Point::new(tile.bounds.x, tile.bounds.y))?;
                canvas.set_size(Size::new(draw_w, draw_h))?;
                if let Some(top_id) = tile.widgets.last().copied() {
                    if let Some((_, widget)) = self.widgets.iter_mut().find(|(id, _)| *id == top_id)
                    {
                        let mut ctx = canvas.context()?;
                        let local_bounds = crate::ui::Rect::new(0.0, 0.0, draw_w, draw_h);
                        widget.draw(&mut ctx, local_bounds, tile.clipped)?;
                    }
                }
            } else {
                canvas.set_visible(false)?;
            }
        }
        Ok(())
    }

    fn render_children(&mut self) -> Result<()> {
        self.tile0.render()?;
        self.tile1.render()?;
        self.tile2.render()?;
        self.tile3.render()?;
        self.tile4.render()?;
        self.tile5.render()?;
        self.tile6.render()?;
        self.tile7.render()?;
        self.tile8.render()?;
        self.root.render()
    }
}

impl UiComponent {
    fn build_widgets(runtime: &UiRuntime) -> Result<Vec<(WidgetId, Box<dyn Widget>)>> {
        let mut out = Vec::<(WidgetId, Box<dyn Widget>)>::new();
        for node in &runtime.plan.widgets {
            let bounds = runtime
                .plan
                .bounds
                .iter()
                .find(|(id, _)| *id == node.id)
                .map(|(_, b)| *b)
                .unwrap_or(crate::ui::Rect::new(0.0, 0.0, 0.0, 0.0));
            let clipped = runtime
                .plan
                .tile_plan
                .tiles
                .iter()
                .find(|tile| tile.widgets.iter().any(|id| *id == node.id))
                .map(|t| t.clipped)
                .unwrap_or(false);
            let slot = TileSlot { bounds, clipped };
            let widget: Box<dyn Widget> = match node.widget_type {
                "Button" => Box::new(Button::init(slot)?),
                "Banner" => Box::new(Banner::init(slot)?),
                _ => Box::new(Container::init(slot)?),
            };
            out.push((node.id, widget));
        }
        Ok(out)
    }

    fn local_to_global(&self, idx: usize, p: Point) -> Point {
        if let Some(tile) = self.runtime.plan.tile_plan.tiles.get(idx) {
            Point::new(p.x + tile.bounds.x, p.y + tile.bounds.y)
        } else {
            p
        }
    }
}

fn expand_size(size: Size) -> Size {
    Size::new(
        size.width + COMPONENT_OVERDRAW_PX,
        size.height + COMPONENT_OVERDRAW_PX,
    )
}

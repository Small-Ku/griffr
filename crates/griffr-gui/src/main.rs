use griffr_gui::widget_tree;
use winio::prelude::*;

#[widget_tree(
    griffr_gui::widget::GradientContainer(flex_direction = Column, flex_grow = 1.0, flex_basis = 600.0, padding = 10.0) {
        griffr_gui::widget::CounterWidget(flex_grow = 1.0, flex_basis = 280.0, margin = 6.0),
        griffr_gui::widget::Banner(flex_grow = 2.0, flex_basis = 320.0, margin = 6.0, clip = ForceClip)
    }
)]
struct MainUi;

fn main() -> Result<()> {
    App::new("rs.compio.griffr.gui")?.run::<MainModel>(())
}

struct MainModel {
    window: Child<Window>,
    ui_component: Child<MainUiComponent>,
}

enum MainMessage {
    Noop,
    Close,
    Redraw,
}

impl Component for MainModel {
    type Error = Error;
    type Event = ();
    type Init<'a> = ();
    type Message = MainMessage;

    async fn init(_init: Self::Init<'_>, _sender: &ComponentSender<Self>) -> Result<Self> {
        init! {
            window: Window = (()) => {
                text: "Griffr GUI",
                size: Size::new(900.0, 640.0),
            }
        }
        let ui_component = Child::<MainUiComponent>::init(&window).await?;
        window.show()?;
        Ok(Self { window, ui_component })
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        start! {
            sender, default: MainMessage::Noop,
            self.window => {
                WindowEvent::Close => MainMessage::Close,
                WindowEvent::Resize | WindowEvent::Move => MainMessage::Redraw,
            },
            self.ui_component => {
                MainUiComponentEvent::Redraw => MainMessage::Redraw,
                MainUiComponentEvent::Target(_) => MainMessage::Noop,
            }
        }
    }

    async fn update_children(&mut self) -> Result<bool> {
        update_children!(self.window, self.ui_component)
    }

    async fn update(
        &mut self,
        message: Self::Message,
        sender: &ComponentSender<Self>,
    ) -> Result<bool> {
        match message {
            MainMessage::Noop => Ok(false),
            MainMessage::Close => {
                sender.output(());
                Ok(false)
            }
            MainMessage::Redraw => {
                let _ = self.window.client_size()?;
                Ok(true)
            }
        }
    }

    fn render(&mut self, _sender: &ComponentSender<Self>) -> Result<()> {
        let csize = self.window.client_size()?;
        self.ui_component.post(MainUiComponentMessage::Resize(csize));
        Ok(())
    }

    fn render_children(&mut self) -> Result<()> {
        self.ui_component.render()?;
        self.window.render()
    }
}

use griffr_gui::widget_tree;
use winio::prelude::*;

#[widget_tree(
    griffr_gui::widget::GradientContainer(flex_direction = Column, flex_grow = 1.0, flex_basis = 600.0, padding = 10.0) {
        griffr_gui::widget::CounterWidget(flex_grow = 1.0, flex_basis = 280.0, margin = 6.0),
        griffr_gui::widget::Banner(flex_grow = 2.0, flex_basis = 320.0, margin = 6.0, clip = ForceClip)
    }
)]
struct MainUi;

#[widget_tree(
    griffr_gui::widget::Container(flex_direction = Column, flex_grow = 0.0, flex_basis = 60.0, padding = 10.0) {
        griffr_gui::widget::Button(flex_grow = 0.0, flex_basis = 40.0, margin = 0.0),
        griffr_gui::widget::Button(flex_grow = 0.0, flex_basis = 40.0, margin = 0.0)
    }
)]
struct SidebarUi;

fn main() -> Result<()> {
    App::new("rs.compio.griffr.gui")?.run::<MainModel>(())
}

struct MainModel {
    window: Child<Window>,
    ui_component: Child<MainUiComponent>,
    sidebar: Child<SidebarUiComponent>,
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
        let sidebar = Child::<SidebarUiComponent>::init(&window).await?;
        window.show()?;
        Ok(Self { window, ui_component, sidebar })
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
            },
            self.sidebar => {
                SidebarUiComponentEvent::Redraw => MainMessage::Redraw,
                SidebarUiComponentEvent::Target(_) => MainMessage::Noop,
            }
        }
    }

    async fn update_children(&mut self) -> Result<bool> {
        update_children!(self.window, self.ui_component, self.sidebar)
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
        let mut root = layout! {
            Grid::from_str("60,1*", "1*").unwrap(),
            self.sidebar => { column: 0, row: 0 },
            self.ui_component => { column: 1, row: 0 },
        };
        root.set_size(csize).unwrap();
        Ok(())
    }

    fn render_children(&mut self) -> Result<()> {
        self.sidebar.render()?;
        self.ui_component.render()?;
        self.window.render()
    }
}

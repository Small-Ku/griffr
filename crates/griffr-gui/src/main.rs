use winio::prelude::*;

fn main() -> Result<()> {
    App::new("rs.compio.winio.example")?.run::<MainModel>(())
}

struct MainModel {
    window: Child<Window>,
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
        // create & initialize the window
        init! {
            window: Window = (()) => {
                text: "Example",
                size: Size::new(800.0, 600.0),
            }
        }
        window.show()?;
        Ok(Self { window })
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        // listen to events
        start! {
            sender, default: MainMessage::Noop,
            self.window => {
                WindowEvent::Close => MainMessage::Close,
                WindowEvent::Move => MainMessage::Redraw,
                WindowEvent::Resize => MainMessage::Redraw,
            }
        }
    }

    async fn update_children(&mut self) -> Result<bool> {
        // update the window
        update_children!(self.window)
    }

    async fn update(
        &mut self,
        message: Self::Message,
        sender: &ComponentSender<Self>,
    ) -> Result<bool> {
        // deal with custom messages
        match message {
            MainMessage::Noop => Ok(false),
            MainMessage::Close => {
                // the root component output stops the application
                sender.output(());
                // need not to call `render`
                Ok(false)
            }
            MainMessage::Redraw => Ok(true),
        }
    }

    fn render(&mut self, _sender: &ComponentSender<Self>) -> Result<()> {
        let csize = self.window.client_size()?;
        // adjust layout and draw widgets here
        Ok(())
    }

    fn render_children(&mut self) -> Result<()> {
        self.window.render()
    }
}

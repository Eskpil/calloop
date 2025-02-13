//! A generic event source wrapping an IO objects or file descriptor
//!
//! You can use this general purpose adapter around file-descriptor backed objects to
//! insert into an [`EventLoop`](crate::EventLoop).
//!
//! The event generated by this [`Generic`] event source are the [`Readiness`](crate::Readiness)
//! notification itself, and the monitored object is provided to your callback as the second
//! argument.
//!
//! ```
//! # extern crate calloop;
//! use calloop::{generic::Generic, Interest, Mode, PostAction};
//!
//! # fn main() {
//! # let mut event_loop = calloop::EventLoop::<()>::try_new()
//! #                .expect("Failed to initialize the event loop!");
//! # let handle = event_loop.handle();
//! # let io_object = 0;
//! handle.insert_source(
//!     // wrap your IO object in a Generic, here we register for read readiness
//!     // in level-triggering mode
//!     Generic::new(io_object, Interest::READ, Mode::Level),
//!     |readiness, io_object, shared_data| {
//!         // The first argument of the callback is a Readiness
//!         // The second is a &mut reference to your object
//!
//!         // your callback needs to return a Result<PostAction, std::io::Error>
//!         // if it returns an error, the event loop will consider this event
//!         // event source as erroring and report it to the user.
//!         Ok(PostAction::Continue)
//!     }
//! );
//! # }
//! ```
//!
//! It can also help you implementing your own event sources: just have
//! these `Generic<_>` as fields of your event source, and delegate the
//! [`EventSource`](crate::EventSource) implementation to them.
//!
//! If you need to directly work with a [`RawFd`](std::os::unix::io::RawFd), rather than an
//! FD-backed object, see [`Generic::from_fd`](Generic#method.from_fd).

use std::{marker::PhantomData, os::unix::io::AsRawFd};

use crate::{EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};

/// A generic event source wrapping a FD-backed type
#[derive(Debug)]
pub struct Generic<F: AsRawFd, E = std::io::Error> {
    /// The wrapped FD-backed type
    pub file: F,
    /// The programmed interest
    pub interest: Interest,
    /// The programmed mode
    pub mode: Mode,
    token: Box<Token>,

    // This allows us to make the associated error and return types generic.
    _error_type: PhantomData<E>,
}

impl<F: AsRawFd> Generic<F, std::io::Error> {
    /// Wrap a FD-backed type into a `Generic` event source that uses
    /// [`std::io::Error`] as its error type.
    pub fn new(file: F, interest: Interest, mode: Mode) -> Generic<F, std::io::Error> {
        Generic {
            file,
            interest,
            mode,
            token: Box::new(Token::invalid()),
            _error_type: PhantomData::default(),
        }
    }

    /// Wrap a FD-backed type into a `Generic` event source using an arbitrary error type.
    pub fn new_with_error<E>(file: F, interest: Interest, mode: Mode) -> Generic<F, E> {
        Generic {
            file,
            interest,
            mode,
            token: Box::new(Token::invalid()),
            _error_type: PhantomData::default(),
        }
    }
}

impl<F: AsRawFd, E> Generic<F, E> {
    /// Unwrap the `Generic` source to retrieve the underlying type
    pub fn unwrap(self) -> F {
        self.file
    }
}

impl<F, E> EventSource for Generic<F, E>
where
    F: AsRawFd,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Event = Readiness;
    type Metadata = F;
    type Ret = Result<PostAction, E>;
    type Error = E;

    fn process_events<C>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: C,
    ) -> Result<PostAction, Self::Error>
    where
        C: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        if token != *self.token {
            return Ok(PostAction::Continue);
        }
        callback(readiness, &mut self.file)
    }

    fn register(&mut self, poll: &mut Poll, token_factory: &mut TokenFactory) -> crate::Result<()> {
        let token = Box::new(token_factory.token());
        unsafe {
            poll.register(
                self.file.as_raw_fd(),
                self.interest,
                self.mode,
                &*token as *const _,
            )?;
        }
        self.token = token;
        Ok(())
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> crate::Result<()> {
        let token = Box::new(token_factory.token());
        unsafe {
            poll.reregister(
                self.file.as_raw_fd(),
                self.interest,
                self.mode,
                &*token as *const _,
            )?;
        }
        self.token = token;
        Ok(())
    }

    fn unregister(&mut self, poll: &mut Poll) -> crate::Result<()> {
        poll.unregister(self.file.as_raw_fd())?;
        self.token = Box::new(Token::invalid());
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::io::{Read, Write};

    use super::Generic;
    use crate::{Dispatcher, Interest, Mode, PostAction};
    #[cfg(unix)]
    #[test]
    fn dispatch_unix() {
        use std::os::unix::net::UnixStream;

        let mut event_loop = crate::EventLoop::try_new().unwrap();

        let handle = event_loop.handle();

        let (mut tx, rx) = UnixStream::pair().unwrap();

        let generic = Generic::new(rx, Interest::READ, Mode::Level);

        let mut dispached = false;

        let _generic_token = handle
            .insert_source(generic, move |readiness, file, d| {
                assert!(readiness.readable);
                // we have not registered for writability
                assert!(!readiness.writable);
                let mut buffer = vec![0; 10];
                let ret = file.read(&mut buffer).unwrap();
                assert_eq!(ret, 6);
                assert_eq!(&buffer[..6], &[1, 2, 3, 4, 5, 6]);

                *d = true;
                Ok(PostAction::Continue)
            })
            .unwrap();

        event_loop
            .dispatch(Some(::std::time::Duration::from_millis(0)), &mut dispached)
            .unwrap();

        assert!(!dispached);

        let ret = tx.write(&[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(ret, 6);
        tx.flush().unwrap();

        event_loop
            .dispatch(Some(::std::time::Duration::from_millis(0)), &mut dispached)
            .unwrap();

        assert!(dispached);
    }

    #[test]
    fn register_deregister_unix() {
        use std::os::unix::net::UnixStream;

        let mut event_loop = crate::EventLoop::try_new().unwrap();

        let handle = event_loop.handle();

        let (mut tx, rx) = UnixStream::pair().unwrap();

        let generic = Generic::new(rx, Interest::READ, Mode::Level);
        let dispatcher = Dispatcher::new(generic, move |_, _, d| {
            *d = true;
            Ok(PostAction::Continue)
        });

        let mut dispached = false;

        let generic_token = handle.register_dispatcher(dispatcher.clone()).unwrap();

        event_loop
            .dispatch(Some(::std::time::Duration::from_millis(0)), &mut dispached)
            .unwrap();

        assert!(!dispached);

        // remove the source, and then write something

        event_loop.handle().remove(generic_token);

        let ret = tx.write(&[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(ret, 6);
        tx.flush().unwrap();

        event_loop
            .dispatch(Some(::std::time::Duration::from_millis(0)), &mut dispached)
            .unwrap();

        // the source has not been dispatched, as the source is no longer here
        assert!(!dispached);

        // insert it again
        let generic = dispatcher.into_source_inner();
        let _generic_token = handle
            .insert_source(generic, move |readiness, file, d| {
                assert!(readiness.readable);
                // we have not registered for writability
                assert!(!readiness.writable);
                let mut buffer = vec![0; 10];
                let ret = file.read(&mut buffer).unwrap();
                assert_eq!(ret, 6);
                assert_eq!(&buffer[..6], &[1, 2, 3, 4, 5, 6]);

                *d = true;
                Ok(PostAction::Continue)
            })
            .unwrap();

        event_loop
            .dispatch(Some(::std::time::Duration::from_millis(0)), &mut dispached)
            .unwrap();

        // the has now been properly dispatched
        assert!(dispached);
    }
}

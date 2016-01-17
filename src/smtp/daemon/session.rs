use nom::IResult;
use super::Config;
use super::connection::{Direction, RecvBuf, SendBuf};
use super::super::protocol::{Command, CommandError, Domain, ExpnParameters, 
                             MailboxDomain, VrfyParameters, Word,};
use ::util::scribe::{Scribe, Scribble};


//------------ Session ------------------------------------------------------

// An SMTP session on top of an SMTP connection
//
pub struct Session<'a> {
    config: &'a Config,
    state: State,
}

impl<'a> Session<'a> {
    pub fn new(config: &'a Config) -> Session<'a> {
        Session {
            config: config,
            state: State::Early,
        }
    }

    pub fn process(&mut self, recv: &mut RecvBuf, send: &mut SendBuf)
                   -> Direction {
        let len = recv.len();
        let (advance, dir) = match Command::parse(recv.as_slice()) {
            IResult::Done(rest, Ok(cmd)) => {
                (len - rest.len(), self.command(cmd, send, rest.len() == len))
            }
            IResult::Done(rest, Err(e)) => {
                Reply::error(send, e);
                (len - rest.len(), Direction::Reply)
            }
            IResult::Error(..) => {
                (0, Direction::Closed)
            }
            IResult::Incomplete(..) => (0, Direction::Receive)
        };
        recv.advance(advance);
        dir
    }

    //--- Generic Command Processing

    /// Process a command.
    ///
    /// If `last` is `true`, this is the last command in a pipelined
    /// sequence.
    ///
    /// Returns the `Direction` to continue with.
    ///
    pub fn command(&mut self, cmd: Command, send: &mut SendBuf, last: bool)
                   -> Direction {
        match self.state {
            State::Early => self.early(cmd, send, last),
            State::Session => self.session(cmd, send, last),
            State::TransactionRcpt => self.transaction_rcpt(cmd, send, last),
            State::Closing => self.closing(cmd, send, last),
            _ => unreachable!()
        }
    }

    /// Process a command in `State::Early`.
    ///
    /// All non-transaction commands are allowed.
    ///
    fn early(&mut self, cmd: Command, send: &mut SendBuf, last: bool)
               -> Direction {
        match cmd {
            Command::Helo(domain) => self.hello(domain, send, last),
            Command::Ehlo(domain) => self.ehello(domain, send, last),
            Command::Vrfy(whom, params) => self.verify(whom, params, send,
                                                       last),
            Command::Expn(whom, params) => self.expand(whom, params, send,
                                                       last),
            Command::Help(what) => self.help(what, send, last),
            Command::Noop | Command::Rset => self.noop(send),
            Command::Quit => self.quit(send),
            _ => {
                Reply::reply(send, 503, (5, 0, 3),
                             b"Please say hello first\r\n");
                Direction::Reply
            }
        }
    }

    /// Process a command in `State::Session`.
    ///
    /// All non-transaction commands plus `MAIL` are allowed.
    ///
    fn session(&mut self, cmd: Command, send: &mut SendBuf, last: bool)
               -> Direction {
        match cmd {
            Command::Helo(domain) => self.hello(domain, send, last),
            Command::Ehlo(domain) => self.ehello(domain, send, last),
            Command::Vrfy(whom, params) => self.verify(whom, params, send,
                                                       last),
            Command::Expn(whom, params) => self.expand(whom, params, send,
                                                       last),
            Command::Help(what) => self.help(what, send, last),
            Command::Noop | Command::Rset => self.noop(send),
            Command::Quit => self.quit(send),
            _ => {
                Reply::reply(send, 502, (5, 0, 2),
                             b"Command not implemented\r\n");
                Direction::Reply
            }
        }
    }

    /// Process a command in `State::TransactionRcpt`.
    ///
    /// Allowed commands are RCPT, DATA and BDAT.
    ///
    fn transaction_rcpt(&mut self, cmd: Command, send: &mut SendBuf,
                        last: bool) -> Direction {
        match cmd {
            Command::Vrfy(whom, params) => self.verify(whom, params, send,
                                                       last),
            Command::Expn(whom, params) => self.expand(whom, params, send,
                                                       last),
            Command::Help(what) => self.help(what, send, last),
            _ => {
                Reply::reply(send, 503, (5, 0, 3),
                             b"Only RCTP, DATA or BDAT allowed now\r\n");
                Direction::Reply
            }
        }
    }

    /// Process a command in `State::Closing`.
    ///
    /// The only allowed command is `Command::Quit`. Everything else gets
    /// a 503.
    ///
    fn closing(&mut self, cmd: Command, send: &mut SendBuf, last: bool)
               -> Direction {
        match cmd {
            Command::Quit => self.quit(send),
            _ => {
                Reply::reply(send, 503, (5, 0, 3),
                             b"Please leave now\r\n");
                Direction::Reply
            }
        }
    }

    //--- Processing of Specific Commands

    fn hello(&mut self, domain: Domain, send: &mut SendBuf, last: bool)
             -> Direction {
        scribble!(&mut Reply::new(send, 205, None),
                  &self.config.hostname, b"\r\n");
        self.state = State::Session;
        Direction::Reply
    }

    fn ehello(&mut self, domain: MailboxDomain, send: &mut SendBuf, last: bool)
              -> Direction {
        scribble!(&mut Reply::new(send, 205, None),
                  &self.config.hostname,
                  b"\r\nEXPN\r\nHELP\r\n8BITMIME\r\nSIZE ",
                  self.config.message_size_limit,
                  b"\r\nCHUNKING\r\nBINARYMIME\r\nPIPELINING\r\nDSN\r\n\
                  ETRN\r\nENHANCEDSTATUSCODES\r\nSTARTTLS\r\nAUTH\r\n\
                  SMTPUTF8\r\n");
        self.state = State::Session;
        Direction::Reply
    }

    fn verify(&mut self, whom: Word, params: VrfyParameters,
              send: &mut SendBuf, last: bool) -> Direction {
        let _ = whom;
        let _ = params;
        Reply::reply(send, 252, (2, 5, 2),
                     b"VRFY administratively disable\r\n");
        Direction::Reply
    }

    fn expand(&mut self, whom: Word, params: ExpnParameters,
              send: &mut SendBuf, last: bool) -> Direction {
        Reply::reply(send, 252, (2, 5, 2),
                     b"EXPN administratively disable\r\n");
        Direction::Reply
    }

    fn help(&mut self, what: Option<Word>, send: &mut SendBuf, last: bool)
            -> Direction {
        Reply::reply(send, 250, (2, 5, 0),
                     b"Some helpful text will appear here soon.\r\n");
        Direction:: Reply
    }

    fn noop(&mut self, send: &mut SendBuf) -> Direction {
        Reply::reply(send, 250, (2, 5, 0), b"OK\r\n");
        Direction::Reply
    }

    fn quit(&mut self, send: &mut SendBuf) -> Direction {
        Reply::reply(send, 221, (2, 0, 0), b"Bye\r\n");
        Direction::Closing
    }
}


//------------ State --------------------------------------------------------

/// The state of an SMTP connection.
///
#[derive(Debug)]
enum State {
    /// No Hello has been received yet
    Early,

    /// A Hello has been received and no mail transaction is going on
    Session,

    /// A mail transaction is beeing prepared (ie., we are collecting the
    /// RCPT commands)
    TransactionRcpt,

    /// A mail transaction's data is being received
    TransactionData,

    /// A mail transaction's data is being received using the BDAT command
    TransactionBdat,

    /// Something went wrong and we are waiting for QUIT
    Closing,
}


//------------ MailTransaction ----------------------------------------------


//------------ Reply --------------------------------------------------------

/// A type to help writing a reply.
///
#[derive(Debug)]
struct Reply<'a> {
    buf: &'a mut SendBuf,
    code: u16,
    status: Option<(u16, u16, u16)>,

    /// Position of the space after the code in the last written prefix.
    sp: usize,

    /// Are we right after a CRLF?
    ///
    /// If yes, the next write starts a new line of a multiline reply.
    crlf: bool,
}

impl<'a> Reply<'a> {
    fn new(send: &'a mut SendBuf, code: u16, status: Option<(u16, u16, u16)>)
           -> Reply<'a> {
        let sp = write_prefix(send, code, status);
        Reply {
            buf: send,
            code: code,
            status: status,
            sp: sp,
            crlf: false,
        }
    }

    fn reply(send: &mut SendBuf, code: u16, status: (u16, u16, u16),
             buf: &[u8]) {
        let mut res = Reply::new(send, code, Some(status));
        scribble!(&mut res, buf);
    }

    fn error(send: &mut SendBuf, err: CommandError) {
        match err {
            CommandError::Unrecognized =>
                Reply::reply(send, 500, (5, 0, 0),
                             b"Command unrecognized\r\n"),
            CommandError::Parameters => 
                Reply::reply(send, 501, (5, 0, 1),
                             b"Syntax error in parameters\r\n"),
        }
    }
}

impl<'a> ::util::scribe::Scribe for Reply<'a> {
    fn scribble_bytes(&mut self, buf: &[u8]) {
        if buf.is_empty() { return; }
        for line in buf.split(|ch| *ch == b'\n') {
            if line.is_empty() { continue }
            if self.crlf {
                self.buf.update(self.sp, b'-');
                self.sp = write_prefix(self.buf, self.code, self.status);
                self.crlf = false;
            }
            self.buf.scribble_bytes(line);
            if line.last() == Some(&b'\r') {
                self.buf.scribble_octet(b'\n');
                self.crlf = true;
            }
        }
    }

    fn scribble_octet(&mut self, ch: u8) {
        if self.crlf {
            self.buf.update(self.sp, b'-');
            self.sp = write_prefix(self.buf, self.code, self.status);
            self.crlf = false;
        }
        self.buf.scribble_octet(ch)
    }
}

/// Writes the prefix for a line.
///
/// Returns the position of the space after the code.
///
fn write_prefix(send: &mut SendBuf, code: u16, status: Option<(u16, u16, u16)>)
                -> usize {
    send.scribble_octet(((code / 100) % 10) as u8 + b'0');
    send.scribble_octet(((code / 10) % 10) as u8 + b'0');
    send.scribble_octet((code % 10) as u8 + b'0');
    let res = send.len();
    send.scribble_octet(b' ');
    match status {
        None => { },
        Some((a, b, c)) => {
            scribble!(send, a, b".", b, b".", c, b" ")
        }
    };
    res
}


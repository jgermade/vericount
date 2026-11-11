//! Texts of the emails the server sends. Like the interface, they are in
//! Spanish; this is the only place on the server with user-facing text.

use super::Mail;

pub fn sign_in_code(to: &str, code: &str, minutes: i64) -> Mail {
    Mail {
        to: to.to_owned(),
        subject: format!("{code} es tu código de acceso a vericounts"),
        body: format!(
            "Tu código de acceso a vericounts es:\n\n    {code}\n\n\
             Caduca en {minutes} minutos y solo sirve una vez.\n\n\
             Si no lo has pedido tú, ignora este correo: nadie puede entrar sin el código.\n"
        ),
    }
}

pub fn invitation(to: &str, invited_by: &str, url: &str) -> Mail {
    Mail {
        to: to.to_owned(),
        subject: "Invitación a vericounts".to_owned(),
        body: format!(
            "{invited_by} te ha invitado a vericounts.\n\n\
             Para crear tu cuenta, entra en {url} y escribe esta dirección de correo. \
             Te enviaremos un código para confirmarla; no hace falta contraseña.\n"
        ),
    }
}

#!/usr/bin/env python3
"""One SMTP transaction. Credentials enter via stdin, diagnostics are allowlisted."""
import base64
import json
import smtplib
import ssl
import sys
from email.message import EmailMessage
from email.policy import SMTP
from email.utils import formataddr, formatdate


def send(value):
    connection = None
    data_started = False
    try:
        for key in ('host', 'sender', 'sender_name', 'recipient', 'subject', 'message_id'):
            if not isinstance(value[key], str) or any(ord(c) < 32 for c in value[key]):
                return {'status': 'failed', 'category': 'configuration'}
        if value['tls'] not in ('starttls', 'implicit'):
            return {'status': 'failed', 'category': 'configuration'}
        message = EmailMessage(policy=SMTP)
        message['From'] = formataddr((value['sender_name'], value['sender']))
        message['To'] = value['recipient']
        message['Subject'] = value['subject']
        message['Message-ID'] = value['message_id']
        message['Date'] = formatdate(localtime=False)
        message.set_content(value['text'])
        logo = value.get('logo_base64')
        html = value['html']
        if logo:
            html = html.replace(f'data:image/png;base64,{logo}', 'cid:vaultcenter-logo')
        message.add_alternative(html, subtype='html')
        if logo:
            message.get_payload()[-1].add_related(
                base64.b64decode(logo, validate=True), maintype='image', subtype='png',
                cid='<vaultcenter-logo>', disposition='inline')
        raw = message.as_bytes()
        context = ssl.create_default_context()
        if value['tls'] == 'implicit':
            connection = smtplib.SMTP_SSL(value['host'], value['port'], timeout=15, context=context, local_hostname='vaultcenter')
        else:
            connection = smtplib.SMTP(value['host'], value['port'], timeout=15, local_hostname='vaultcenter')
        connection.ehlo()
        if value['tls'] == 'starttls':
            connection.starttls(context=context)
            connection.ehlo()
        if value['username']:
            connection.login(value['username'], value['password'])
        code, _ = connection.mail(value['sender'])
        if code != 250:
            return {'status': 'transient' if 400 <= code < 500 else 'failed', 'category': 'sender_rejected'}
        code, _ = connection.rcpt(value['recipient'])
        if code not in (250, 251):
            return {'status': 'transient' if 400 <= code < 500 else 'failed', 'category': 'recipient_rejected'}
        data_started = True
        code, _ = connection.data(raw)
        if code != 250:
            return {'status': 'transient' if 400 <= code < 500 else 'failed', 'category': 'message_rejected'}
        return {'status': 'sent', 'category': 'smtp_accepted'}
    except ssl.SSLCertVerificationError:
        return {'status': 'failed', 'category': 'certificate'}
    except smtplib.SMTPAuthenticationError as error:
        return {'status': 'transient' if 400 <= error.smtp_code < 500 else 'failed', 'category': 'authentication'}
    except smtplib.SMTPResponseException as error:
        return {'status': 'transient' if 400 <= error.smtp_code < 500 else 'failed', 'category': 'smtp_rejected'}
    except (smtplib.SMTPServerDisconnected, OSError):
        return {'status': 'uncertain' if data_started else 'transient', 'category': 'connection'}
    except (smtplib.SMTPException, ValueError, TypeError, KeyError):
        return {'status': 'uncertain' if data_started else 'failed', 'category': 'configuration'}
    finally:
        if connection is not None:
            # QUIT failures after a 250 DATA reply must not downgrade acceptance.
            try:
                connection.close()
            except OSError:
                pass


if __name__ == '__main__':
    try:
        payload = json.loads(sys.stdin.buffer.read(8 * 1024 * 1024))
        print(json.dumps(send(payload)))
    except Exception:
        print(json.dumps({'status': 'failed', 'category': 'configuration'}))

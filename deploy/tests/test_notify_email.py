import importlib.util
from pathlib import Path
import smtplib
import types
import unittest
from unittest.mock import patch

path=Path(__file__).resolve().parents[1]/'cold-vtl/notify-email.py'
if path.exists():
    spec=importlib.util.spec_from_file_location('notify_email',path)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
else:module=types.SimpleNamespace(send=lambda data:{'status':'not_implemented'})

class SMTPFixture:
    failure=None
    messages=[]
    def __init__(self,*args,**kwargs):pass
    def ehlo(self):return 250,b'ok'
    def starttls(self,**kwargs):return 220,b'ok'
    def login(self,*args):return 235,b'ok'
    def mail(self,*args):return 250,b'ok'
    def rcpt(self,*args):return 250,b'ok'
    def data(self,raw):
        self.messages.append(raw)
        if self.failure:raise self.failure
        return 250,b'accepted'
    def close(self):pass

class MailTest(unittest.TestCase):
    def setUp(self):
        SMTPFixture.failure=None;SMTPFixture.messages=[]
        self.payload=dict(host='localhost',port=587,tls='starttls',username='',password='',sender='vaultcenter@example.test',sender_name='VaultCenter',recipient='ops@example.test',subject='Export terminé',text='Texte lisible',html='<p>Texte lisible</p>',message_id='<delivery-1@vaultcenter.invalid>')
    def send(self):
        with patch('smtplib.SMTP',SMTPFixture):return module.send(self.payload)
    def test_multipart_message_and_acceptance(self):
        self.assertEqual(self.send()['status'],'sent')
        self.assertIn(b'multipart/alternative',SMTPFixture.messages[0])
        self.assertIn(b'delivery-1@vaultcenter.invalid',SMTPFixture.messages[0])
    def test_logo_is_embedded_and_html_uses_its_content_id(self):
        import base64
        from email import policy
        from email.parser import BytesParser
        image=(Path(__file__).resolve().parents[2]/'ui/public/brand/vaultcenter.png').read_bytes()
        logo=base64.b64encode(image).decode()
        self.payload.update(logo_base64=logo,html=f'<img src="data:image/png;base64,{logo}" alt="">')
        self.assertEqual(self.send()['status'],'sent')
        message=BytesParser(policy=policy.default).parsebytes(SMTPFixture.messages[0])
        self.assertIn('cid:vaultcenter-logo',message.get_body(preferencelist=('html',)).get_content())
        self.assertNotIn('data:image',message.get_body(preferencelist=('html',)).get_content())
        images=[part for part in message.walk() if part.get_content_type()=='image/png']
        self.assertEqual(len(images),1)
        self.assertEqual(images[0]['Content-ID'],'<vaultcenter-logo>')
        self.assertEqual(images[0].get_payload(decode=True),image)
        self.assertEqual(images[0].get_content_disposition(),'inline')

    def test_lost_data_reply_is_uncertain_without_server_text(self):
        SMTPFixture.failure=smtplib.SMTPServerDisconnected('secret response')
        result=self.send()
        self.assertEqual(result['status'],'uncertain')
        self.assertNotIn('secret',str(result))
    def test_explicit_temporary_rejection_can_retry(self):
        SMTPFixture.failure=smtplib.SMTPDataError(451,b'secret response')
        self.assertEqual(self.send()['status'],'transient')
    def test_reject_header_injection_before_connecting(self):
        self.payload['subject']='ok\r\nBcc: victim@example.test'
        self.assertEqual(self.send()['status'],'failed')
        self.assertEqual(SMTPFixture.messages,[])
    def test_temporary_authentication_failure_can_retry(self):
        self.payload['username']='account';self.payload['password']='secret'
        with patch.object(SMTPFixture,'login',side_effect=smtplib.SMTPAuthenticationError(454,b'temporary')):
            self.assertEqual(self.send()['status'],'transient')

    def test_plaintext_transport_is_rejected(self):
        self.payload['tls']='none'
        self.assertEqual(self.send()['status'],'failed')

if __name__=='__main__':unittest.main()

class LocalTLSMailTest(unittest.TestCase):
    """Exercise real smtplib, MIME and certificate validation on loopback only."""
    def test_real_tls_delivery(self):
        import email.parser
        import os
        import socket
        import ssl
        import subprocess
        import tempfile
        import threading
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            (root/'openssl.cnf').write_text('[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:TRUE\n')
            subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(root/'key.pem'),'-out',str(root/'cert.pem'),'-days','1','-config',str(root/'openssl.cnf')],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            for mode in ('implicit','starttls'):
                context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(root/'cert.pem',root/'key.pem')
                listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen();listener.settimeout(5)
                captured=[];errors=[];commands=[]
                def serve():
                    try:
                        conn,_=listener.accept();conn.settimeout(5)
                        if mode=='implicit':conn=context.wrap_socket(conn,server_side=True)
                        stream=conn.makefile('rb');conn.sendall(b'220 localhost test\r\n')
                        while True:
                            line=stream.readline()
                            if not line:break
                            commands.append(line.decode().strip())
                            line=line.upper()
                            if line.startswith(b'EHLO'):conn.sendall(b'250-localhost\r\n250 STARTTLS\r\n')
                            elif line.startswith(b'STARTTLS'):
                                conn.sendall(b'220 ready\r\n');stream.close();conn=context.wrap_socket(conn,server_side=True);stream=conn.makefile('rb')
                            elif line.startswith((b'MAIL',b'RCPT')):conn.sendall(b'250 ok\r\n')
                            elif line.startswith(b'DATA'):
                                conn.sendall(b'354 send\r\n');body=b''
                                while True:
                                    item=stream.readline()
                                    if item in (b'.\r\n',b''):break
                                    body+=item
                                captured.append(body);conn.sendall(b'250 accepted\r\n')
                            else:conn.sendall(b'250 ok\r\n')
                        stream.close();conn.close()
                    except Exception as error:errors.append(str(error))
                    finally:listener.close()
                thread=threading.Thread(target=serve,daemon=True);thread.start()
                payload=dict(host='localhost',port=listener.getsockname()[1],tls=mode,username='',password='',sender='vaultcenter@example.test',sender_name='VaultCenter',recipient='ops@example.test',subject='Restauration terminée',text='Fichiers vérifiés',html='<p>Fichiers vérifiés</p>',message_id='<real-local@vaultcenter.invalid>')
                with patch.dict(os.environ,{'SSL_CERT_FILE':str(root/'cert.pem')}):result=module.send(payload)
                thread.join(6)
                self.assertFalse(thread.is_alive());self.assertEqual(errors,[],f'{mode}: {result}, commands={commands}');self.assertEqual(result['status'],'sent',f'{mode}: {result}, commands={commands}');self.assertEqual(len(captured),1)
                mail=email.parser.BytesParser(policy=__import__('email.policy',fromlist=['default']).default).parsebytes(captured[0])
                self.assertEqual(mail['To'],'ops@example.test');self.assertEqual(mail['Subject'],'Restauration terminée')
                self.assertIn('Fichiers vérifiés',mail.get_body(preferencelist=('plain',)).get_content())
                self.assertIn('<p>Fichiers vérifiés</p>',mail.get_body(preferencelist=('html',)).get_content())

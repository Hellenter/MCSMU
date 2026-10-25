#!/usr/bin/env bash
# Настройка домена для веб-панели MC Server Manager на внешнем сервере (например, ex1.ru).
# Панель работает на домашнем ПК и пробрасывается сюда туннелем:
#   ssh -R 127.0.0.1:8080:127.0.0.1:8080 minecraft@ex1.ru
# nginx принимает https://ДОМЕН и передаёт запросы в этот туннель.
#
# Запуск (Debian/Ubuntu, от root):
#   sudo bash setup-nginx.sh mc.example.ru [порт=8080] [email для Let's Encrypt]
# Перед запуском A-запись домена должна указывать на IP этого сервера.
set -euo pipefail

DOMAIN="${1:-}"
PORT="${2:-8080}"
EMAIL="${3:-}"
if [[ -z "$DOMAIN" ]]; then
  echo "Использование: sudo bash $0 ДОМЕН [ПОРТ] [EMAIL]" >&2
  exit 1
fi
if [[ $EUID -ne 0 ]]; then
  echo "Запустите от root (sudo)." >&2
  exit 1
fi

echo "== Устанавливаю nginx и certbot"
apt-get update -q
apt-get install -y -q nginx certbot python3-certbot-nginx

CONF="/etc/nginx/sites-available/mcsm-${DOMAIN}"
echo "== Пишу ${CONF}"
cat > "$CONF" <<NGINX
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};

    # загрузка модов и скачивание бэкапов
    client_max_body_size 1g;

    location / {
        proxy_pass http://127.0.0.1:${PORT};
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_read_timeout 600s;
        proxy_send_timeout 600s;
        proxy_buffering off;
        proxy_request_buffering off;
    }

    # пока туннель с домашнего ПК не подключён
    error_page 502 504 = @offline;
    location @offline {
        default_type text/html;
        return 503 '<!doctype html><meta charset="utf-8"><title>Панель недоступна</title><body style="font-family:sans-serif;padding:40px"><h2>Панель сейчас недоступна</h2><p>Компьютер с серверами выключен или туннель панели не открыт.</p></body>';
    }
}
NGINX
ln -sf "$CONF" "/etc/nginx/sites-enabled/mcsm-${DOMAIN}"
nginx -t
systemctl reload nginx

echo "== Получаю сертификат Let's Encrypt"
if [[ -n "$EMAIL" ]]; then
  certbot --nginx -d "$DOMAIN" --redirect --non-interactive --agree-tos -m "$EMAIL"
else
  certbot --nginx -d "$DOMAIN" --redirect --non-interactive --agree-tos --register-unsafely-without-email
fi

echo
echo "Готово: https://${DOMAIN}"
echo "Теперь на домашнем ПК откройте панель → Администрирование → «Туннель панели» и нажмите «Открыть»."
echo "Команда туннеля: ssh -R 127.0.0.1:${PORT}:127.0.0.1:<порт панели> <пользователь>@<этот сервер>"

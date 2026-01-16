# NIFTY Spread Strategy - Deployment Guide

## Azure VM Deployment

### Prerequisites

1. **Azure VM Requirements**:
   - Ubuntu 22.04 LTS or Windows Server 2022
   - Minimum: 2 vCPU, 4GB RAM
   - Recommended: Standard B2s or B2ms

2. **Software Requirements**:
   - Python 3.10+
   - Redis Server
   - Git

### Setup Steps

#### 1. Install Dependencies (Ubuntu)

```bash
# Update system
sudo apt update && sudo apt upgrade -y

# Install Python and pip
sudo apt install python3 python3-pip python3-venv -y

# Install Redis
sudo apt install redis-server -y
sudo systemctl enable redis-server
sudo systemctl start redis-server

# Verify Redis
redis-cli ping  # Should return PONG
```

#### 2. Clone and Setup Project

```bash
# Clone repository
git clone <your-repo-url> trading_with_shoonya
cd trading_with_shoonya

# Create virtual environment
python3 -m venv venv
source venv/bin/activate

# Install dependencies
pip install -r requirements.txt
```

#### 3. Configure Credentials

Create `cred.yml`:

```yaml
# Shoonya API Credentials
user: "YOUR_USER_ID"
pwd: "YOUR_PASSWORD"
totp_pin: "YOUR_TOTP_SECRET"
vc: "YOUR_VENDOR_CODE"
apikey: "YOUR_API_KEY"
imei: "YOUR_IMEI"

# Telegram Alerts (Optional)
telegram:
  enabled: true
  bot_token: "YOUR_BOT_TOKEN"
  chat_id: "YOUR_CHAT_ID"
```

#### 4. Test the Setup

```bash
# Activate virtual environment
source venv/bin/activate

# Run analysis only (no trades)
python nifty_spread_runner.py --analyze-only

# Run in paper trade mode
python nifty_spread_runner.py --paper-trade --capital 500000
```

### Running as a Service (24x7)

#### Option 1: Systemd Service (Recommended)

Create `/etc/systemd/system/nifty-spread.service`:

```ini
[Unit]
Description=NIFTY Spread Strategy Bot
After=network.target redis.service

[Service]
Type=simple
User=azureuser
WorkingDirectory=/home/azureuser/trading_with_shoonya
ExecStart=/home/azureuser/trading_with_shoonya/venv/bin/python nifty_spread_runner.py --capital 500000
Restart=always
RestartSec=10
StandardOutput=append:/var/log/nifty-spread/output.log
StandardError=append:/var/log/nifty-spread/error.log

[Install]
WantedBy=multi-user.target
```

```bash
# Create log directory
sudo mkdir -p /var/log/nifty-spread
sudo chown azureuser:azureuser /var/log/nifty-spread

# Enable and start service
sudo systemctl daemon-reload
sudo systemctl enable nifty-spread
sudo systemctl start nifty-spread

# Check status
sudo systemctl status nifty-spread

# View logs
journalctl -u nifty-spread -f
```

#### Option 2: Docker (Alternative)

Create `Dockerfile`:

```dockerfile
FROM python:3.12-slim

WORKDIR /app

# Install dependencies
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# Copy application
COPY . .

# Run the bot
CMD ["python", "nifty_spread_runner.py", "--capital", "500000"]
```

Create `docker-compose.yml`:

```yaml
version: '3.8'

services:
  redis:
    image: redis:7-alpine
    restart: always
    volumes:
      - redis_data:/data

  nifty-spread:
    build: .
    restart: always
    depends_on:
      - redis
    volumes:
      - ./cred.yml:/app/cred.yml:ro
      - ./logs:/app/logs
    environment:
      - REDIS_HOST=redis

volumes:
  redis_data:
```

```bash
# Build and run
docker-compose up -d

# View logs
docker-compose logs -f nifty-spread
```

### Configuration Options

| Argument | Default | Description |
|----------|---------|-------------|
| `--capital` | 500000 | Trading capital in INR |
| `--max-risk` | 2.0 | Maximum risk per trade (%) |
| `--max-lots` | 5 | Maximum lots per trade |
| `--symbol` | NIFTY | Trading symbol |
| `--paper-trade` | False | Paper trading mode |
| `--analyze-only` | False | Only show analysis |
| `--log-level` | INFO | Logging level |

### Monitoring

#### Health Check Endpoints (Future Enhancement)

For production, consider adding a health check API:

```python
# Add to nifty_spread_runner.py
from http.server import HTTPServer, BaseHTTPRequestHandler
import json

class HealthHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/health':
            self.send_response(200)
            self.send_header('Content-type', 'application/json')
            self.end_headers()
            status = {
                "status": "running",
                "positions": len(runner.active_positions),
                "pnl": runner.total_pnl
            }
            self.wfile.write(json.dumps(status).encode())
```

### Telegram Setup

1. Create a bot via @BotFather on Telegram
2. Get the bot token
3. Start a chat with your bot
4. Get your chat ID by messaging @userinfobot
5. Add credentials to `cred.yml`

### Troubleshooting

| Issue | Solution |
|-------|----------|
| Redis connection error | Ensure Redis is running: `sudo systemctl status redis` |
| API login failed | Check credentials in `cred.yml` |
| No trades executed | Check if market is open and VIX levels |
| Telegram not working | Verify bot token and chat ID |

### Security Considerations

1. **Credentials**: Never commit `cred.yml` to git
2. **Firewall**: Only allow necessary ports (SSH)
3. **Updates**: Keep system and dependencies updated
4. **Monitoring**: Set up Azure alerts for VM health

### Estimated Costs

| Resource | Specification | Monthly Cost (USD) |
|----------|---------------|-------------------|
| Azure VM | Standard B2s | ~$30-40 |
| Storage | 30GB SSD | ~$5 |
| Network | Basic | ~$5 |
| **Total** | | **~$40-50** |

---

## Quick Start

```bash
# One-liner to start in paper trade mode
python nifty_spread_runner.py --paper-trade --analyze-only --log-level DEBUG
```

## Support

Check logs in `./logs/` directory for debugging.

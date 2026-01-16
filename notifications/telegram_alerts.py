"""
Telegram notifications for trade alerts
"""
import logging
from typing import Optional
import datetime
import requests

logger = logging.getLogger(__name__)


class TelegramAlerter:
    """
    Send trade alerts via Telegram bot.
    
    Setup:
    1. Create a bot via @BotFather on Telegram
    2. Get the bot token
    3. Get your chat ID (message @userinfobot)
    """
    
    def __init__(
        self,
        bot_token: Optional[str] = None,
        chat_id: Optional[str] = None,
        enabled: bool = True
    ):
        """
        Initialize Telegram alerter.
        
        Args:
            bot_token: Telegram bot token
            chat_id: Chat ID to send messages to
            enabled: Whether alerts are enabled
        """
        self.bot_token = bot_token
        self.chat_id = chat_id
        self.enabled = enabled and bot_token and chat_id
        
        if not self.enabled:
            logger.warning("Telegram alerts disabled - missing token or chat_id")
        else:
            logger.info("Telegram alerts enabled")
    
    @classmethod
    def from_config(cls, config_file: str = "cred.yml") -> "TelegramAlerter":
        """
        Create alerter from config file.
        
        Args:
            config_file: Path to YAML config file
        
        Returns:
            TelegramAlerter instance
        """
        try:
            import yaml
            with open(config_file, 'r') as f:
                config = yaml.safe_load(f)
            
            telegram_config = config.get('telegram', {})
            return cls(
                bot_token=telegram_config.get('bot_token'),
                chat_id=telegram_config.get('chat_id'),
                enabled=telegram_config.get('enabled', True)
            )
        except Exception as e:
            logger.error(f"Error loading Telegram config: {e}")
            return cls(enabled=False)
    
    def send_message(self, message: str, parse_mode: str = "HTML") -> bool:
        """
        Send a message via Telegram.
        
        Args:
            message: Message text
            parse_mode: HTML or Markdown
        
        Returns:
            True if sent successfully
        """
        if not self.enabled:
            logger.debug(f"Telegram disabled, message: {message}")
            return False
        
        try:
            url = f"https://api.telegram.org/bot{self.bot_token}/sendMessage"
            payload = {
                "chat_id": self.chat_id,
                "text": message,
                "parse_mode": parse_mode
            }
            
            response = requests.post(url, json=payload, timeout=10)
            
            if response.status_code == 200:
                logger.debug("Telegram message sent")
                return True
            else:
                logger.error(f"Telegram error: {response.text}")
                return False
                
        except Exception as e:
            logger.error(f"Error sending Telegram message: {e}")
            return False
    
    def send_trade_entry(
        self,
        strategy: str,
        symbol: str,
        short_strike: str,
        long_strike: str,
        premium: float,
        max_profit: float,
        max_loss: float,
        quantity: int
    ):
        """Send trade entry notification"""
        message = f"""
🟢 <b>TRADE ENTRY</b>

📊 Strategy: {strategy}
🏷 Symbol: {symbol}

📈 Short: {short_strike}
📉 Long: {long_strike}
📦 Qty: {quantity}

💰 Premium: ₹{premium:.2f}
✅ Max Profit: ₹{max_profit:.2f}
❌ Max Loss: ₹{max_loss:.2f}

⏰ {datetime.datetime.now().strftime('%H:%M:%S')}
"""
        return self.send_message(message)
    
    def send_trade_exit(
        self,
        strategy: str,
        symbol: str,
        exit_reason: str,
        pnl: float,
        pnl_percent: float
    ):
        """Send trade exit notification"""
        emoji = "🟢" if pnl >= 0 else "🔴"
        message = f"""
{emoji} <b>TRADE EXIT</b>

📊 Strategy: {strategy}
🏷 Symbol: {symbol}
📋 Reason: {exit_reason}

💵 P&L: ₹{pnl:+,.2f} ({pnl_percent:+.2f}%)

⏰ {datetime.datetime.now().strftime('%H:%M:%S')}
"""
        return self.send_message(message)
    
    def send_alert(self, level: str, message: str, action: str):
        """Send risk alert notification"""
        level_emoji = {
            "INFO": "ℹ️",
            "WARNING": "⚠️",
            "CRITICAL": "🚨",
            "EMERGENCY": "🆘"
        }.get(level, "📢")
        
        msg = f"""
{level_emoji} <b>{level} ALERT</b>

{message}

🔧 Action: {action}

⏰ {datetime.datetime.now().strftime('%H:%M:%S')}
"""
        return self.send_message(msg)
    
    def send_daily_summary(
        self,
        total_trades: int,
        winning_trades: int,
        total_pnl: float,
        positions_open: int
    ):
        """Send end of day summary"""
        win_rate = (winning_trades / total_trades * 100) if total_trades > 0 else 0
        emoji = "🟢" if total_pnl >= 0 else "🔴"
        
        message = f"""
📊 <b>DAILY SUMMARY</b>

📈 Total Trades: {total_trades}
✅ Winners: {winning_trades}
📊 Win Rate: {win_rate:.1f}%

{emoji} P&L: ₹{total_pnl:+,.2f}

📦 Open Positions: {positions_open}

📅 {datetime.datetime.now().strftime('%Y-%m-%d')}
"""
        return self.send_message(message)
    
    def send_strategy_signal(
        self,
        strategy_type: str,
        confidence: float,
        rsi: float,
        vix: float,
        spot: float,
        reason: str
    ):
        """Send strategy signal notification"""
        emoji = "📈" if strategy_type == "BCS" else "📉" if strategy_type == "BPS" else "⏸"
        
        message = f"""
{emoji} <b>STRATEGY SIGNAL</b>

🎯 Recommendation: {strategy_type}
📊 Confidence: {confidence:.0f}%

📍 Spot: {spot:,.2f}
📉 RSI: {rsi:.1f}
📊 VIX: {vix:.2f}

💡 {reason}

⏰ {datetime.datetime.now().strftime('%H:%M:%S')}
"""
        return self.send_message(message)
    
    def send_startup_message(self):
        """Send bot startup notification"""
        message = f"""
🚀 <b>TRADING BOT STARTED</b>

NIFTY Spread Strategy Bot is now running.

⏰ {datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')}
"""
        return self.send_message(message)
    
    def send_shutdown_message(self, reason: str = "Manual shutdown"):
        """Send bot shutdown notification"""
        message = f"""
🛑 <b>TRADING BOT STOPPED</b>

Reason: {reason}

⏰ {datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')}
"""
        return self.send_message(message)

"""
NIFTY Weekly Options Spread Strategy Runner

This is the main entry point for the 24x7 automated trading bot.
It executes Bull Put Spread (BPS) or Bull Call Spread (BCS) based on
market indicators (RSI, VIX, EMA).

Usage:
    python nifty_spread_runner.py --capital 500000 --max-risk 2.0
    python nifty_spread_runner.py --paper-trade  # Paper trading mode
    python nifty_spread_runner.py --analyze-only  # Only show signals
"""
import argparse
import logging
import signal
import sys
import time
import datetime
from typing import Optional, List

# Configure path for imports
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from utils import configure_logger
from client_shoonya import ShoonyaApiPy
from const import LOT_SIZE

# Import strategy modules
from strategies import SpreadSelector, StrategyType, BullPutSpread, BullCallSpread, SpreadOrder
from indicators import VIXMonitor
from risk import PositionSizer, CrashProtector, StopLossManager, StopLossConfig
from scheduler import MarketHours, JobScheduler, JobType
from notifications import TelegramAlerter

logger = logging.getLogger(__name__)


class NiftySpreadRunner:
    """
    Main runner for NIFTY spread strategies.
    
    Features:
    - Automatic strategy selection (BPS vs BCS)
    - Position sizing based on risk
    - Real-time stop loss monitoring
    - Crash protection
    - 24x7 operation with scheduling
    - Telegram alerts
    """
    
    def __init__(
        self,
        capital: float = 500000.0,
        max_risk_percent: float = 2.0,
        max_lots: int = 5,
        symbol: str = "NIFTY",
        paper_trade: bool = False,
        cred_file: str = "cred.yml"
    ):
        """
        Initialize the spread runner.
        
        Args:
            capital: Trading capital
            max_risk_percent: Maximum risk per trade
            max_lots: Maximum lots per trade
            symbol: Trading symbol
            paper_trade: If True, don't place real orders
            cred_file: Path to credentials file
        """
        self.capital = capital
        self.symbol = symbol
        self.paper_trade = paper_trade
        self.cred_file = cred_file
        
        # State tracking
        self.running = False
        self.active_positions: List[SpreadOrder] = []
        self.total_pnl = 0.0
        self.trade_count = 0
        self.win_count = 0
        
        # Initialize API
        logger.info("Initializing Shoonya API...")
        self.api = ShoonyaApiPy(cred_file=cred_file)
        
        # Initialize components
        self.market_hours = MarketHours()
        self.selector = SpreadSelector(self.api, symbol)
        self.position_sizer = PositionSizer(capital, max_risk_percent, max_lots)
        self.crash_protector = CrashProtector(
            self.api, 
            symbol,
            on_alert=self._on_crash_alert
        )
        
        # Stop loss manager
        sl_config = StopLossConfig(
            sl_multiplier=2.0,
            target_percent=0.50,
            trailing_enabled=True,
            time_exit_enabled=True,
            time_exit_hour=15,
            time_exit_minute=15
        )
        self.sl_manager = StopLossManager(
            self.api,
            config=sl_config,
            on_exit=self._on_position_exit,
            check_interval=10
        )
        
        # Telegram alerts
        self.alerter = TelegramAlerter.from_config(cred_file)
        
        # Strategies
        self.bps = BullPutSpread(self.api, symbol)
        self.bcs = BullCallSpread(self.api, symbol)
        
        # Scheduler
        self.scheduler = JobScheduler(self.market_hours)
        self._setup_jobs()
        
        logger.info(
            f"NiftySpreadRunner initialized | Capital: ₹{capital:,.0f} | "
            f"Max Risk: {max_risk_percent}% | Paper Trade: {paper_trade}"
        )
    
    def _setup_jobs(self):
        """Setup scheduled jobs"""
        # Strategy scan - every 5 minutes
        self.scheduler.add_interval_job(
            "strategy_scan",
            self._job_strategy_scan,
            interval_seconds=300,
            job_type=JobType.STRATEGY_SCAN
        )
        
        # Risk check - every 2 minutes
        self.scheduler.add_interval_job(
            "risk_check",
            self._job_risk_check,
            interval_seconds=120,
            job_type=JobType.RISK_CHECK
        )
        
        # EOD summary
        self.scheduler.add_daily_job(
            "eod_summary",
            self._job_eod_summary,
            run_time=datetime.time(15, 35),
            job_type=JobType.EOD_SUMMARY
        )
    
    def _on_crash_alert(self, trigger):
        """Handle crash protection alerts"""
        self.alerter.send_alert(
            trigger.level.value,
            trigger.message,
            trigger.action
        )
        
        # Emergency exit if needed
        if trigger.action == "EXIT_ALL_POSITIONS":
            self._emergency_exit("Crash protection triggered")
    
    def _on_position_exit(self, position, exit_reason):
        """Handle position exit from stop loss manager"""
        logger.info(f"Position exit: {position.position_id} - {exit_reason.value}")
        
        # Calculate P&L
        pnl = (position.entry_premium - position.current_premium) * position.quantity
        pnl_percent = ((position.entry_premium - position.current_premium) / position.entry_premium) * 100
        
        self.total_pnl += pnl
        self.trade_count += 1
        if pnl > 0:
            self.win_count += 1
        
        # Send alert
        self.alerter.send_trade_exit(
            position.strategy_type,
            self.symbol,
            exit_reason.value,
            pnl,
            pnl_percent
        )
        
        # Remove from active positions
        self.active_positions = [
            p for p in self.active_positions 
            if str(id(p)) != position.position_id
        ]
        
        # Update capital
        self.position_sizer.update_capital(self.capital + pnl)
        self.capital += pnl
    
    def _job_strategy_scan(self):
        """Scheduled job: Scan for entry opportunities"""
        logger.debug("Running strategy scan...")
        
        # Check if we can enter
        can_enter, reason = self.market_hours.can_enter_trade()
        if not can_enter:
            logger.debug(f"Cannot enter: {reason}")
            return
        
        # Check crash protection
        can_trade, triggers = self.crash_protector.run_all_checks(
            open_positions=len(self.active_positions),
            current_risk=sum(p.max_loss for p in self.active_positions),
            capital=self.capital
        )
        
        if not can_trade:
            logger.info("Crash protector blocking trades")
            return
        
        # Get strategy signal
        signal = self.selector.select_strategy()
        
        if signal.strategy == StrategyType.NO_TRADE:
            logger.info(f"No trade signal: {signal.reason}")
            return
        
        if signal.strategy == StrategyType.WAIT:
            logger.info(f"Wait signal: {signal.reason}")
            return
        
        if signal.confidence < 60:
            logger.info(f"Low confidence ({signal.confidence}%): {signal.reason}")
            return
        
        # Send signal alert
        self.alerter.send_strategy_signal(
            signal.strategy.value,
            signal.confidence,
            signal.rsi_value,
            signal.vix_value,
            signal.spot_price,
            signal.reason
        )
        
        # Execute trade
        self._execute_strategy(signal)
    
    def _job_risk_check(self):
        """Scheduled job: Check risk parameters"""
        logger.debug("Running risk check...")
        
        # Check for emergency exit conditions
        should_exit, reason = self.crash_protector.should_exit_all()
        if should_exit:
            self._emergency_exit(reason)
    
    def _job_eod_summary(self):
        """Scheduled job: End of day summary"""
        logger.info("Generating EOD summary...")
        
        self.alerter.send_daily_summary(
            total_trades=self.trade_count,
            winning_trades=self.win_count,
            total_pnl=self.total_pnl,
            positions_open=len(self.active_positions)
        )
    
    def _execute_strategy(self, signal):
        """Execute the selected strategy"""
        logger.info(f"Executing {signal.strategy.value} with {signal.confidence}% confidence")
        
        # Select strategy
        if signal.strategy == StrategyType.BPS:
            strategy = self.bps
            spread_order = strategy.create_spread_order(
                quantity=1,
                otm_percent=1.5,
                spread_width_points=100
            )
        else:
            strategy = self.bcs
            spread_order = strategy.create_spread_order(
                quantity=1,
                buy_strike_offset=0,
                spread_width_points=100
            )
        
        if not spread_order:
            logger.error("Failed to create spread order")
            return
        
        # Calculate position size
        is_credit = signal.strategy == StrategyType.BPS
        position_size = self.position_sizer.calculate_for_spread(
            spread_width=spread_order.spread_width,
            premium=abs(spread_order.net_premium),
            lot_size=LOT_SIZE[self.symbol],
            is_credit_spread=is_credit
        )
        
        # Validate position
        existing_risk = sum(p.max_loss for p in self.active_positions)
        is_valid, validation_msg = self.position_sizer.validate_position(
            position_size, existing_risk
        )
        
        if not is_valid:
            logger.warning(f"Position validation failed: {validation_msg}")
            return
        
        # Adjust quantity
        spread_order.long_leg.quantity = position_size.quantity
        spread_order.short_leg.quantity = position_size.quantity
        spread_order.max_profit = spread_order.net_premium * position_size.quantity if is_credit else (spread_order.spread_width - abs(spread_order.net_premium)) * position_size.quantity
        spread_order.max_loss = position_size.max_risk
        
        # Paper trade or real trade
        if self.paper_trade:
            logger.info(f"[PAPER] Would execute: {spread_order.strategy_name}")
            logger.info(f"[PAPER] Short: {spread_order.short_leg.trading_symbol} @ {spread_order.short_leg.ltp}")
            logger.info(f"[PAPER] Long: {spread_order.long_leg.trading_symbol} @ {spread_order.long_leg.ltp}")
        else:
            success = strategy.place_spread_order(spread_order)
            if not success:
                logger.error("Failed to place spread order")
                return
        
        # Add to monitoring
        position_id = str(id(spread_order))
        self.sl_manager.add_position(
            position_id=position_id,
            entry_premium=abs(spread_order.net_premium),
            long_token=spread_order.long_leg.token,
            short_token=spread_order.short_leg.token,
            quantity=position_size.quantity,
            exchange=self.bps.exchange,
            strategy_type="BPS" if is_credit else "BCS"
        )
        
        self.active_positions.append(spread_order)
        
        # Send alert
        self.alerter.send_trade_entry(
            strategy=spread_order.strategy_name,
            symbol=self.symbol,
            short_strike=spread_order.short_leg.trading_symbol,
            long_strike=spread_order.long_leg.trading_symbol,
            premium=abs(spread_order.net_premium),
            max_profit=spread_order.max_profit,
            max_loss=spread_order.max_loss,
            quantity=position_size.quantity
        )
        
        logger.info(f"Trade executed: {spread_order.strategy_name}")
    
    def _emergency_exit(self, reason: str):
        """Emergency exit all positions"""
        logger.critical(f"EMERGENCY EXIT: {reason}")
        self.alerter.send_alert("EMERGENCY", f"Emergency exit triggered: {reason}", "EXIT_ALL")
        
        self.sl_manager.emergency_exit_all(reason)
        
        # TODO: Implement actual order exit via API
        # For now, just clear positions
        self.active_positions = []
    
    def analyze(self):
        """Run analysis only (no trading)"""
        logger.info("Running analysis mode...")
        
        # Print market status
        self.market_hours.print_status()
        
        # Print strategy signal
        signal = self.selector.print_analysis()
        
        # Show sample orders
        print("\n--- SAMPLE BPS ORDER ---")
        bps_order = self.bps.create_spread_order(quantity=1)
        
        print("\n--- SAMPLE BCS ORDER ---")
        bcs_order = self.bcs.create_spread_order(quantity=1)
        
        return signal
    
    def start(self):
        """Start the trading bot"""
        logger.info("Starting NIFTY Spread Runner...")
        self.running = True
        
        # Send startup alert
        self.alerter.send_startup_message()
        
        # Start stop loss monitoring
        self.sl_manager.start_monitoring()
        
        # Start scheduler
        self.scheduler.start()
        
        logger.info("Bot is now running. Press Ctrl+C to stop.")
        
        # Setup signal handlers
        signal.signal(signal.SIGINT, self._signal_handler)
        signal.signal(signal.SIGTERM, self._signal_handler)
        
        # Main loop
        try:
            while self.running:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
        
        self.stop()
    
    def stop(self, reason: str = "Manual shutdown"):
        """Stop the trading bot"""
        logger.info(f"Stopping bot: {reason}")
        self.running = False
        
        # Stop components
        self.sl_manager.stop_monitoring()
        self.scheduler.stop()
        
        # Send shutdown alert
        self.alerter.send_shutdown_message(reason)
        
        # Print final summary
        print("\n" + "="*50)
        print("FINAL SUMMARY")
        print("="*50)
        print(f"Total Trades: {self.trade_count}")
        print(f"Winning Trades: {self.win_count}")
        print(f"Win Rate: {(self.win_count/self.trade_count*100) if self.trade_count > 0 else 0:.1f}%")
        print(f"Total P&L: ₹{self.total_pnl:+,.2f}")
        print(f"Open Positions: {len(self.active_positions)}")
        print("="*50)
        
        logger.info("Bot stopped")
    
    def _signal_handler(self, signum, frame):
        """Handle shutdown signals"""
        logger.info(f"Received signal {signum}")
        self.running = False


def parse_arguments():
    """Parse command line arguments"""
    parser = argparse.ArgumentParser(
        description="NIFTY Weekly Options Spread Strategy Runner",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python nifty_spread_runner.py --capital 500000
  python nifty_spread_runner.py --paper-trade --analyze-only
  python nifty_spread_runner.py --max-risk 1.5 --max-lots 3
        """
    )
    
    parser.add_argument(
        "--capital",
        type=float,
        default=500000.0,
        help="Trading capital (default: 500000)"
    )
    
    parser.add_argument(
        "--max-risk",
        type=float,
        default=2.0,
        help="Maximum risk per trade as %% of capital (default: 2.0)"
    )
    
    parser.add_argument(
        "--max-lots",
        type=int,
        default=5,
        help="Maximum lots per trade (default: 5)"
    )
    
    parser.add_argument(
        "--symbol",
        type=str,
        default="NIFTY",
        choices=["NIFTY", "BANKNIFTY", "FINNIFTY"],
        help="Trading symbol (default: NIFTY)"
    )
    
    parser.add_argument(
        "--paper-trade",
        action="store_true",
        help="Enable paper trading (no real orders)"
    )
    
    parser.add_argument(
        "--analyze-only",
        action="store_true",
        help="Only show analysis, don't start bot"
    )
    
    parser.add_argument(
        "--cred-file",
        type=str,
        default="cred.yml",
        help="Path to credentials file (default: cred.yml)"
    )
    
    parser.add_argument(
        "--log-level",
        type=str,
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging level (default: INFO)"
    )
    
    return parser.parse_args()


def main():
    """Main entry point"""
    args = parse_arguments()
    
    # Configure logging
    log_level = getattr(logging, args.log_level)
    configure_logger(log_level, prefix_log_file="nifty_spread")
    
    logger.info("="*60)
    logger.info("NIFTY SPREAD STRATEGY RUNNER")
    logger.info("="*60)
    
    # Create runner
    runner = NiftySpreadRunner(
        capital=args.capital,
        max_risk_percent=args.max_risk,
        max_lots=args.max_lots,
        symbol=args.symbol,
        paper_trade=args.paper_trade,
        cred_file=args.cred_file
    )
    
    if args.analyze_only:
        # Just run analysis
        runner.analyze()
    else:
        # Start the bot
        runner.start()


if __name__ == "__main__":
    main()

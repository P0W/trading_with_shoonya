"""
Job scheduler for 24x7 automated trading operation
"""
import logging
from typing import Callable, Dict, Optional, Any
from dataclasses import dataclass
import datetime
import threading
import time
from enum import Enum

logger = logging.getLogger(__name__)


class JobType(Enum):
    """Types of scheduled jobs"""
    INDICATOR_UPDATE = "indicator_update"
    STRATEGY_SCAN = "strategy_scan"
    POSITION_MONITOR = "position_monitor"
    RISK_CHECK = "risk_check"
    EOD_SUMMARY = "eod_summary"
    DATA_DOWNLOAD = "data_download"


@dataclass
class ScheduledJob:
    """Scheduled job definition"""
    job_id: str
    job_type: JobType
    func: Callable
    interval_seconds: Optional[int] = None
    run_time: Optional[datetime.time] = None
    enabled: bool = True
    last_run: Optional[datetime.datetime] = None
    next_run: Optional[datetime.datetime] = None
    run_count: int = 0
    error_count: int = 0


class JobScheduler:
    """
    Simple job scheduler for trading automation.
    
    Supports:
    - Interval-based jobs (run every X seconds)
    - Time-based jobs (run at specific time daily)
    - Market-hours aware scheduling
    
    For production, consider using APScheduler with this as a wrapper.
    """
    
    def __init__(self, market_hours=None):
        """
        Initialize scheduler.
        
        Args:
            market_hours: MarketHours instance for trading day awareness
        """
        self.jobs: Dict[str, ScheduledJob] = {}
        self._running = False
        self._scheduler_thread: Optional[threading.Thread] = None
        
        if market_hours is None:
            from scheduler.market_hours import MarketHours
            market_hours = MarketHours()
        self.market_hours = market_hours
    
    def add_interval_job(
        self,
        job_id: str,
        func: Callable,
        interval_seconds: int,
        job_type: JobType = JobType.POSITION_MONITOR,
        run_immediately: bool = False
    ):
        """
        Add a job that runs at fixed intervals.
        
        Args:
            job_id: Unique job identifier
            func: Function to run
            interval_seconds: Seconds between runs
            job_type: Type of job
            run_immediately: Run job immediately when added
        """
        now = datetime.datetime.now()
        next_run = now if run_immediately else now + datetime.timedelta(seconds=interval_seconds)
        
        job = ScheduledJob(
            job_id=job_id,
            job_type=job_type,
            func=func,
            interval_seconds=interval_seconds,
            next_run=next_run
        )
        
        self.jobs[job_id] = job
        logger.info(f"Added interval job: {job_id} (every {interval_seconds}s)")
    
    def add_daily_job(
        self,
        job_id: str,
        func: Callable,
        run_time: datetime.time,
        job_type: JobType = JobType.EOD_SUMMARY
    ):
        """
        Add a job that runs at a specific time daily.
        
        Args:
            job_id: Unique job identifier
            func: Function to run
            run_time: Time to run (daily)
            job_type: Type of job
        """
        now = datetime.datetime.now()
        run_datetime = datetime.datetime.combine(now.date(), run_time)
        
        if run_datetime <= now:
            run_datetime += datetime.timedelta(days=1)
        
        job = ScheduledJob(
            job_id=job_id,
            job_type=job_type,
            func=func,
            run_time=run_time,
            next_run=run_datetime
        )
        
        self.jobs[job_id] = job
        logger.info(f"Added daily job: {job_id} (at {run_time})")
    
    def remove_job(self, job_id: str):
        """Remove a scheduled job"""
        if job_id in self.jobs:
            del self.jobs[job_id]
            logger.info(f"Removed job: {job_id}")
    
    def enable_job(self, job_id: str):
        """Enable a job"""
        if job_id in self.jobs:
            self.jobs[job_id].enabled = True
            logger.info(f"Enabled job: {job_id}")
    
    def disable_job(self, job_id: str):
        """Disable a job"""
        if job_id in self.jobs:
            self.jobs[job_id].enabled = False
            logger.info(f"Disabled job: {job_id}")
    
    def run_job(self, job: ScheduledJob) -> bool:
        """
        Execute a job.
        
        Returns:
            True if successful, False otherwise
        """
        try:
            logger.debug(f"Running job: {job.job_id}")
            job.func()
            job.last_run = datetime.datetime.now()
            job.run_count += 1
            return True
            
        except Exception as e:
            logger.error(f"Error in job {job.job_id}: {e}")
            job.error_count += 1
            return False
    
    def _calculate_next_run(self, job: ScheduledJob):
        """Calculate next run time for a job"""
        now = datetime.datetime.now()
        
        if job.interval_seconds:
            job.next_run = now + datetime.timedelta(seconds=job.interval_seconds)
        
        elif job.run_time:
            next_run = datetime.datetime.combine(now.date(), job.run_time)
            if next_run <= now:
                next_run += datetime.timedelta(days=1)
            job.next_run = next_run
    
    def _scheduler_loop(self):
        """Main scheduler loop"""
        logger.info("Scheduler loop started")
        
        while self._running:
            try:
                now = datetime.datetime.now()
                
                for job_id, job in self.jobs.items():
                    if not job.enabled:
                        continue
                    
                    if job.next_run and now >= job.next_run:
                        # Check market hours for trading jobs
                        if job.job_type in [JobType.STRATEGY_SCAN, JobType.POSITION_MONITOR]:
                            if not self.market_hours.is_trading_day():
                                self._calculate_next_run(job)
                                continue
                        
                        success = self.run_job(job)
                        self._calculate_next_run(job)
                
                time.sleep(1)  # Check every second
                
            except Exception as e:
                logger.error(f"Scheduler loop error: {e}")
                time.sleep(5)
        
        logger.info("Scheduler loop stopped")
    
    def start(self):
        """Start the scheduler"""
        if self._running:
            logger.warning("Scheduler already running")
            return
        
        self._running = True
        self._scheduler_thread = threading.Thread(
            target=self._scheduler_loop,
            daemon=True,
            name="JobScheduler"
        )
        self._scheduler_thread.start()
        logger.info("Scheduler started")
    
    def stop(self):
        """Stop the scheduler"""
        self._running = False
        if self._scheduler_thread:
            self._scheduler_thread.join(timeout=10)
        logger.info("Scheduler stopped")
    
    def get_job_status(self) -> Dict[str, Dict]:
        """Get status of all jobs"""
        status = {}
        
        for job_id, job in self.jobs.items():
            status[job_id] = {
                "type": job.job_type.value,
                "enabled": job.enabled,
                "last_run": job.last_run.isoformat() if job.last_run else None,
                "next_run": job.next_run.isoformat() if job.next_run else None,
                "run_count": job.run_count,
                "error_count": job.error_count
            }
        
        return status
    
    def print_status(self):
        """Print scheduler status"""
        status = self.get_job_status()
        
        print("\n" + "="*70)
        print("SCHEDULER STATUS")
        print("="*70)
        print(f"Running: {self._running}")
        print(f"Jobs: {len(self.jobs)}")
        print("-"*70)
        
        for job_id, info in status.items():
            enabled = "✓" if info["enabled"] else "✗"
            print(f"{enabled} {job_id}: {info['type']}")
            print(f"   Runs: {info['run_count']} | Errors: {info['error_count']}")
            print(f"   Next: {info['next_run']}")
        
        print("="*70 + "\n")


def setup_trading_scheduler(
    shoonya_api,
    strategy_callback: Callable,
    monitor_callback: Callable,
    risk_callback: Callable,
    eod_callback: Optional[Callable] = None
) -> JobScheduler:
    """
    Setup a scheduler with standard trading jobs.
    
    Args:
        shoonya_api: API instance
        strategy_callback: Function to check for entry signals
        monitor_callback: Function to monitor positions
        risk_callback: Function to check risk parameters
        eod_callback: Optional EOD summary function
    
    Returns:
        Configured JobScheduler
    """
    scheduler = JobScheduler()
    
    # Strategy scan - every 5 minutes during entry window
    scheduler.add_interval_job(
        "strategy_scan",
        strategy_callback,
        interval_seconds=300,
        job_type=JobType.STRATEGY_SCAN
    )
    
    # Position monitoring - every 30 seconds
    scheduler.add_interval_job(
        "position_monitor",
        monitor_callback,
        interval_seconds=30,
        job_type=JobType.POSITION_MONITOR
    )
    
    # Risk check - every 2 minutes
    scheduler.add_interval_job(
        "risk_check",
        risk_callback,
        interval_seconds=120,
        job_type=JobType.RISK_CHECK
    )
    
    # EOD summary at 3:35 PM
    if eod_callback:
        scheduler.add_daily_job(
            "eod_summary",
            eod_callback,
            run_time=datetime.time(15, 35),
            job_type=JobType.EOD_SUMMARY
        )
    
    return scheduler

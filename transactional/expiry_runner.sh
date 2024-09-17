#!/bin/bash

# Define the associative array mapping of days to indices and quantities
declare -A indices
indices[1]="BANKEX 30"
indices[2]="FINNIFTY 50"
indices[3]="BANKNIFTY 30"
indices[4]="NIFTY 50"
indices[5]="SENSEX 20"

# Set the timezone to Asia/Kolkata
export TZ=Asia/Kolkata

# Get the current day of the week (1=Monday, 2=Tuesday, ..., 5=Friday) in IST
day_of_week=$(date +%u)

# Check if today is a valid trading day (Monday to Friday)
if [ "$day_of_week" -ge 1 ] && [ "$day_of_week" -le 5 ]; then
    # Extract the index and quantity for today from the associative array
    IFS=' ' read -r index qty <<< "${indices[$day_of_week]}"

    # Check if index and qty are set
    if [ -z "$index" ] || [ -z "$qty" ]; then
        echo "Index or quantity not set for today. Exiting."
        exit 1
    fi

    # Define the project directory
    project_dir=~/projects/trading_with_shoonya

    # Navigate to the project directory
    cd "$project_dir/transactional" || { echo "Error: Failed to navigate to project directory"; exit 1; }

    # Activate the virtual environment
    source ../bin/activate || { echo "Error: Failed to activate virtual environment"; exit 1; }

    # Set the PYTHONPATH
    export PYTHONPATH="$project_dir:$PYTHONPATH"

    # Run the Python script with the appropriate arguments
    python shoonya_transaction.py --index "$index" --qty "$qty" --same-premium --target-mtm 1500 || { echo "Error: Failed to run Python script"; exit 1; }
fi

## Crontab for running the script on weekdays at 10:30 AM IST
## 30 4 * * 1-5 ~/projects/trading_with_shoonya/transactional/expiry_runner.sh > ~/projects/trading_with_shoonya/transactional/expiry_runner.log 2>&1

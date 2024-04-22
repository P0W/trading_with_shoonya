"""Log file parser to extract PnL at each timestamp"""

## Requires openpyxl, xlsxwriter
import glob
import re
from datetime import datetime

import pandas as pd
import tqdm


def parse_log_file(file_name):
    """Parse the log file and return the PnL at each timestamp"""
    timestamp_pattern = r"\d{2}/\d{2}/\d{4}\|\d{2}:\d{2}:\d{2}\.\d{3}"
    total_pattern = r'"Total":\s*(\S+)'
    ## read and parse the log file in one go
    pnl = {}
    timestamp = None
    with open(file_name, encoding="utf-8") as f:
        for line in f:
            timestamp_match = re.search(timestamp_pattern, line)
            ## check if timestamp is present
            if timestamp_match:
                timestamp_str = timestamp_match.group()
                timestamp = datetime.strptime(timestamp_str, "%d/%m/%Y|%H:%M:%S.%f")
                ## get only the time part as string
                timestamp = timestamp.strftime("%H:%M:%S")
            ## check if the line has 'Total' in it
            total = re.search(total_pattern, line)
            if total and timestamp:
                total = total.group(1)
                ## strip off the quotes and comma, if present
                total = total.replace('"', "").replace(",", "")
                pnl[timestamp] = float(total)
    return pnl


## pylint: disable=too-many-locals, too-many-statements
def main(file_names: list):
    """Main function to test the log parser"""
    summary = []
    results = {}
    day_wise_pnl = {}
    pbar = tqdm.tqdm(file_names)
    with pd.ExcelWriter("pnl.xlsx", engine="xlsxwriter") as writer:
        for file_name in pbar:
            pnl = parse_log_file(file_name)
            pbar.set_description(f"Parsing {file_name:20s}")
            if not pnl:
                pbar.set_description(f"Skipping {file_name} as no PnL found")
                continue
            results[file_name] = pnl

            ## form sheet name from file_name, strip the path and extension
            sheet_name = file_name.split("\\")[-1].split(".")[0]
            ## strip off "shoonya_transaction_" from the sheet name
            sheet_name = sheet_name.replace("shoonya_transaction_", "")
            date_str = sheet_name.split("_")[-1]

            ## Write to xlsx file
            df = pd.DataFrame(pnl.items(), columns=["Time", "PnL"])

            # Add the last PnL for this sheet to the summary
            last_pnl = df.iloc[-1]["PnL"]
            summary.append((sheet_name, last_pnl))

            if date_str not in day_wise_pnl:
                day_wise_pnl[date_str] = last_pnl
            else:
                day_wise_pnl[date_str] += last_pnl

            pbar.set_description(f"Writing {sheet_name:20s}")

        # Write the summary to the first sheet
        summary_df = pd.DataFrame(summary, columns=["Sheet", "Last PnL"])
        summary_df.to_excel(writer, sheet_name="Summary", index=False)

        # Calculate statistics and store them in individual cells
        max_loss = summary_df["Last PnL"].where(summary_df["Last PnL"] < 0).min()
        max_profit = summary_df["Last PnL"].where(summary_df["Last PnL"] > 0).max()
        win_ratio = (summary_df["Last PnL"] > 0).mean()
        total_days = len(summary_df)
        win_count = (summary_df["Last PnL"] > 0).sum()
        loss_count = (summary_df["Last PnL"] < 0).sum()
        total_pnl = summary_df["Last PnL"].sum()
        ## nifty wins counts if Sheet name contains "NIFTY" and PnL > 0
        indices = [
            "NIFTY",
            "FINNIFTY",
            "BANKNIFTY",
            "SENSEX",
            "CRUDEOIL",
            "MIDCPNIFTY",
            "USDINR",
        ]
        indices_stats = {}
        for index in indices:
            wins = summary_df[
                summary_df["Sheet"].str.startswith(index) & (summary_df["Last PnL"] > 0)
            ].shape[0]
            loss = summary_df[
                summary_df["Sheet"].str.startswith(index) & (summary_df["Last PnL"] < 0)
            ].shape[0]
            win_ratio = wins / (wins + loss) if wins + loss > 0 else 0
            pnl = summary_df[summary_df["Sheet"].str.startswith(index)][
                "Last PnL"
            ].sum()
            indices_stats[index] = {
                "Wins": wins,
                "Loss": loss,
                "Win Ratio": win_ratio,
                "PnL": pnl,
            }

        ## Write the statistics to the summary sheet at E12
        start_row = 12
        ## Write the column headers
        writer.sheets["Summary"].write(start_row - 1, 4, "Index")
        writer.sheets["Summary"].write(start_row - 1, 5, "Wins")
        writer.sheets["Summary"].write(start_row - 1, 6, "Loss")
        writer.sheets["Summary"].write(start_row - 1, 7, "Win Ratio")
        writer.sheets["Summary"].write(start_row - 1, 8, "PnL")
        for index, stats in indices_stats.items():
            writer.sheets["Summary"].write(start_row, 4, index)
            writer.sheets["Summary"].write(start_row, 5, stats["Wins"])
            writer.sheets["Summary"].write(start_row, 6, stats["Loss"])
            writer.sheets["Summary"].write(start_row, 7, stats["Win Ratio"])
            writer.sheets["Summary"].write(start_row, 8, stats["PnL"])
            start_row += 1

        ## Write the overall statistics to the summary sheet
        start_row = 12 + len(indices_stats)
        writer.sheets["Summary"].write(start_row, 4, "Total Days")
        writer.sheets["Summary"].write(start_row, 5, total_days)
        writer.sheets["Summary"].write(start_row + 1, 4, "Win Count")
        writer.sheets["Summary"].write(start_row + 1, 5, win_count)
        writer.sheets["Summary"].write(start_row + 2, 4, "Loss Count")
        writer.sheets["Summary"].write(start_row + 2, 5, loss_count)
        writer.sheets["Summary"].write(start_row + 3, 4, "Win Ratio")
        writer.sheets["Summary"].write(start_row + 3, 5, win_ratio)
        writer.sheets["Summary"].write(start_row + 4, 4, "Max Profit")
        writer.sheets["Summary"].write(start_row + 4, 5, max_profit)
        writer.sheets["Summary"].write(start_row + 5, 4, "Max Loss")
        writer.sheets["Summary"].write(start_row + 5, 5, max_loss)
        writer.sheets["Summary"].write(start_row + 6, 4, "Total PnL")
        writer.sheets["Summary"].write(start_row + 6, 5, total_pnl)

        print("Summary written to Summary sheet")
        # Create a new chart object.
        chart = writer.book.add_chart({"type": "bar"})

        # Configure the series of the chart from the dataframe data.
        chart.add_series(
            {
                "categories": ["Summary", 1, 0, len(summary_df), 0],
                "values": ["Summary", 1, 1, len(summary_df), 1],
            }
        )

        # Configure the chart axes.
        chart.set_x_axis({"name": "Sheet"})
        chart.set_y_axis({"name": "Last PnL", "major_gridlines": {"visible": False}})

        # Insert the chart into the worksheet.
        summary_sheet = writer.sheets["Summary"]
        summary_sheet.insert_chart("J2", chart)

        ## Write the day wise PnL to the Datewise sheet
        day_wise_df = pd.DataFrame(day_wise_pnl.items(), columns=["Date", "PnL"])
        day_wise_df.to_excel(writer, sheet_name="Datewise", index=False)
        win_days_count = day_wise_df[day_wise_df["PnL"] > 0].shape[0]
        loss_days_count = day_wise_df[day_wise_df["PnL"] < 0].shape[0]
        win_ratio = (
            win_days_count / (win_days_count + loss_days_count)
            if win_days_count + loss_days_count > 0
            else 0
        )
        total_days = len(day_wise_df)
        total_pnl = day_wise_df["PnL"].sum()
        max_profit = day_wise_df["PnL"].max()
        max_loss = day_wise_df["PnL"].min()
        start_row = 12
        writer.sheets["Datewise"].write(start_row, 4, "Total Days")
        writer.sheets["Datewise"].write(start_row, 5, total_days)
        writer.sheets["Datewise"].write(start_row + 1, 4, "Win Days")
        writer.sheets["Datewise"].write(start_row + 1, 5, win_days_count)
        writer.sheets["Datewise"].write(start_row + 2, 4, "Loss Days")
        writer.sheets["Datewise"].write(start_row + 2, 5, loss_days_count)
        writer.sheets["Datewise"].write(start_row + 3, 4, "Win Ratio")
        writer.sheets["Datewise"].write(start_row + 3, 5, win_ratio)
        writer.sheets["Datewise"].write(start_row + 4, 4, "Max Profit")
        writer.sheets["Datewise"].write(start_row + 4, 5, max_profit)
        writer.sheets["Datewise"].write(start_row + 5, 4, "Max Loss")
        writer.sheets["Datewise"].write(start_row + 5, 5, max_loss)
        writer.sheets["Datewise"].write(start_row + 6, 4, "Total PnL")
        writer.sheets["Datewise"].write(start_row + 6, 5, total_pnl)
        print("Datewise written to Datewise sheet")

        # Now write the other sheets
        for file_name, pnl in results.items():
            sheet_name = file_name.split("\\")[-1].split(".")[0]
            sheet_name = sheet_name.replace("shoonya_transaction_", "")
            df = pd.DataFrame(pnl.items(), columns=["Time", "PnL"])
            df.to_excel(writer, sheet_name=sheet_name, index=False)

            # Create a chart object
            workbook = writer.book
            chart = workbook.add_chart({"type": "line"})

            # Configure the chart from the dataframe data.
            chart.add_series(
                {
                    "categories": [sheet_name, 1, 0, len(df), 0],
                    "values": [sheet_name, 1, 1, len(df), 1],
                    "name": "PnL",
                }
            )

            # Insert the chart into the worksheet.
            worksheet = writer.sheets[sheet_name]
            worksheet.insert_chart("D2", chart)


if __name__ == "__main__":
    logs = glob.glob(".\\logs\\shoonya_transaction_*_202*.log")
    ## sort files by the timestamp in the file name
    logs.sort(key=lambda x: int(x.split("_")[-1].split(".")[0]))

    main(logs)

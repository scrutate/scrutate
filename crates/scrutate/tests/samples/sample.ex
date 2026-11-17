defmodule Billing.Invoice do
  @moduledoc "Invoices."
  import Billing.Tax

  @doc "Total of lines."
  def total(%{lines: lines}) do
    Enum.sum(lines)
  end

  defp helper(x), do: x * 2
  defmacro debug(expr), do: expr
end

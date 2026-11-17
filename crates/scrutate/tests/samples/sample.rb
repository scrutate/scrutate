# Ruby sample
require 'json'
module Billing
  class Invoice
    # Total of all lines.
    def total
      lines.sum(&:amount)
    end

    def self.build(attrs)
      new(attrs)
    end
  end
end

def helper(x)
  puts x
end

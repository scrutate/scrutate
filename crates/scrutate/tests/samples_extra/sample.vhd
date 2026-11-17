library ieee;
use ieee.std_logic_1164.all;

entity blinker is
  port (clk : in std_logic; led : out std_logic);
end entity blinker;

architecture rtl of blinker is
  signal state : std_logic := '0';
begin
  process (clk)
  begin
    if rising_edge(clk) then
      state <= not state;
    end if;
  end process;
  led <= state;
end architecture rtl;

package util is
  function double(x : integer) return integer;
end package util;

package body util is
  function double(x : integer) return integer is
  begin
    return x * 2;
  end function double;

  function quad(x : integer) return integer is
  begin
    return double(double(x));
  end function quad;
end package body util;

unit Demo;
interface
type
  TPoint = record
    X: Integer;
  end;
  TGreeter = class
    function Greet(const Name: string): string;
  end;
function Add(A, B: Integer): Integer;
implementation
{ Adds two numbers. }
function Add(A, B: Integer): Integer;
begin
  Result := A + B;
end;
function TGreeter.Greet(const Name: string): string;
begin
  Add(1, 2);
  Result := 'hi ' + Name;
end;
end.

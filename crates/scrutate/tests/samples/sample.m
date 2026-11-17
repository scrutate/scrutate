% MATLAB sample
% Greets.
function out = greet(name)
  out = ['hi ' name];
  helper(1);
end
function y = helper(x)
  y = x * 2;
end

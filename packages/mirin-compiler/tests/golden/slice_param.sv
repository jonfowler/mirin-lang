module drop_low #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [(n - 1)-1:0] result
);
    assign result = x[(n - 1):1];
endmodule

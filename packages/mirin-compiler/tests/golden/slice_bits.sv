module s (
    input  logic [15:0] x,
    output logic [3:0] result
);
    assign result = x[7:4];
endmodule

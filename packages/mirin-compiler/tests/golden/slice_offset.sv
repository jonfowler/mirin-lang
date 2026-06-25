module s (
    input  logic [15:0] x,
    input  logic [3:0] i,
    output logic [3:0] result
);
    assign result = x[i +: 4];
endmodule
